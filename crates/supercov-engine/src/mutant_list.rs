//! A run's list of mutants, whatever their language
//! (`docs/design/mutation-drivers.md`).
//!
//! Each language's enumerator writes the same `mutants.json`: every listed
//! file's mutants in the order their switches number them, each with its
//! span, mutator, descriptor and the points that decide its reach. A file's
//! language is `languages[file]`, or JavaScript where the list does not name
//! one, as every list was before other languages listed theirs. Each
//! language's mutant set is `mutantSets[language]`; a list without
//! `mutantSets` is JavaScript's alone, its set `mutantSet`. A list of
//! JavaScript alone writes neither, and reads as it always did.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// The language of a file a list does not name a language for.
pub const JAVASCRIPT: &str = "javascript";
/// Rust's name in a mutant list.
pub const RUST: &str = "rust";

/// Java and Kotlin, which share PIT's mutant set (`jvm_mutants`).
pub const JVM: &str = "jvm";

/// The version of `mutants.json`. It leaves out what a reader works out
/// again: a mutant's `id`, from its file, span, mutator and descriptor, and
/// of a long `original` all but what is shown of it (see
/// `js_weak_mutation::abbreviated`).
pub(crate) const MUTANTS_SCHEMA_VERSION: u32 = 1;

/// `mutants.json`: every instrumented file's mutants, or none when the run
/// did not enumerate them (`mutantSet` null).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(from = "StoredMutants", into = "StoredMutants")]
pub struct MutantsFile {
    pub schema_version: u32,
    /// The mutant set, or every listed language's joined: what a report
    /// names. `None` where the run listed no mutants.
    pub mutant_set: Option<String>,
    /// Each listed language's mutant set, where a language other than
    /// JavaScript listed mutants; empty otherwise.
    pub mutant_sets: BTreeMap<String, String>,
    /// Each listed file's language, where it is not JavaScript.
    pub languages: BTreeMap<String, String>,
    pub files: BTreeMap<String, Vec<CandidateMutant>>,
    /// Of a merged run: the tests of the runs merged into it that listed no
    /// mutants. Their hits say what ran; they recorded no values.
    pub tests_without_values: Vec<String>,
}

impl MutantsFile {
    /// One language's list: its mutant set and its files' mutants.
    pub fn listed(
        language: &str,
        mutant_set: &str,
        files: BTreeMap<String, Vec<CandidateMutant>>,
    ) -> Self {
        let mut list = Self::unlisted();
        list.mutant_set = Some(mutant_set.to_owned());
        if language != JAVASCRIPT {
            list.mutant_sets
                .insert(language.to_owned(), mutant_set.to_owned());
            list.languages = files
                .keys()
                .map(|file| (file.clone(), language.to_owned()))
                .collect();
        }
        list.files = files;
        list
    }

    /// A list that lists nothing: a run measured without its mutants.
    pub fn unlisted() -> Self {
        Self {
            schema_version: MUTANTS_SCHEMA_VERSION,
            mutant_set: None,
            mutant_sets: BTreeMap::new(),
            languages: BTreeMap::new(),
            files: BTreeMap::new(),
            tests_without_values: Vec::new(),
        }
    }

    /// The language of a listed file.
    pub fn language_of(&self, file: &str) -> &str {
        self.languages.get(file).map_or(JAVASCRIPT, String::as_str)
    }

    /// Each listed language's mutant set.
    pub fn sets(&self) -> BTreeMap<String, String> {
        if !self.mutant_sets.is_empty() {
            return self.mutant_sets.clone();
        }
        self.mutant_set
            .iter()
            .map(|set| (JAVASCRIPT.to_owned(), set.clone()))
            .collect()
    }

    /// `other`'s files added to this list's: a file both list keeps this
    /// list's mutants. Each language keeps its set; a language the two list
    /// with different sets did not enumerate the same mutants, and `Err` says
    /// which.
    pub fn merge(mut self, other: MutantsFile) -> Result<MutantsFile, String> {
        let mut sets = self.sets();
        for (language, set) in other.sets() {
            match sets.get(&language) {
                Some(kept) if *kept != set => {
                    return Err(format!(
                        "its {language} mutants are those of {set}, and the other's those of {kept}"
                    ));
                }
                Some(_) => {}
                None => {
                    sets.insert(language, set);
                }
            }
        }
        for (file, mutants) in other.files {
            if self.files.contains_key(&file) {
                continue;
            }
            if let Some(language) = other.languages.get(&file) {
                self.languages.insert(file.clone(), language.clone());
            }
            self.files.insert(file, mutants);
        }
        self.tests_without_values.extend(other.tests_without_values);
        self.tests_without_values.sort();
        self.tests_without_values.dedup();
        // Two unlisted lists list nothing.
        self.mutant_set =
            (!sets.is_empty()).then(|| sets.values().cloned().collect::<Vec<_>>().join(", "));
        // A list of JavaScript alone names no sets, as it always did.
        self.mutant_sets = if sets.keys().all(|language| language == JAVASCRIPT) {
            BTreeMap::new()
        } else {
            sets
        };
        Ok(self)
    }
}

/// The language a source file is listed under, by its extension: what
/// routes a file measured without its mutants to its language's enumerator.
pub fn language_of_path(path: &str) -> Option<&'static str> {
    let extension = std::path::Path::new(path).extension()?.to_string_lossy();
    Some(match &*extension {
        "js" | "mjs" | "cjs" | "jsx" | "ts" | "mts" | "cts" | "tsx" => JAVASCRIPT,
        "py" => "python",
        "rs" => RUST,
        "rb" => "ruby",
        "go" => "go",
        "java" | "kt" | "kts" | "scala" | "groovy" => "jvm",
        _ => return None,
    })
}

/// A run's mutants listed from its sources after the run, and the files
/// changed since, whose mutants could not be listed.
/// cargo-mutants' mutants of each Rust file a run measured
/// (`rust_mutants::list`).
fn list_rust_mutants(
    root: &std::path::Path,
    recorded: &BTreeMap<String, std::collections::BTreeSet<String>>,
) -> ListedMutants {
    let files = recorded.iter().collect::<Vec<_>>();
    let listed = crate::threads::parallel_map(&files, |(file, points)| {
        let source = std::fs::read_to_string(root.join(file)).ok()?;
        crate::rust_mutants::list(file, &source, points).or_else(|| {
            // A crate in a directory of the project (Rust compiled to wasm
            // that JavaScript tests load) names its points by its files'
            // paths in its Cargo workspace.
            file.match_indices('/').find_map(|(at, _)| {
                root.join(&file[..at])
                    .join("Cargo.toml")
                    .is_file()
                    .then(|| crate::rust_mutants::list(&file[at + 1..], &source, points))
                    .flatten()
            })
        })
    });
    let mut mutants = BTreeMap::new();
    let mut changed = Vec::new();
    for ((file, _), listed) in files.into_iter().zip(listed) {
        let file = file.clone();
        match listed {
            Some(list) if list.is_empty() => {}
            Some(list) => {
                mutants.insert(file, list);
            }
            None => changed.push(file),
        }
    }
    ListedMutants {
        mutants: MutantsFile::listed(RUST, crate::rust_mutants::MUTANT_SET, mutants),
        changed,
    }
}

/// The mutants a Rust run measured with `SUPERCOV_MUTANTS=1` lists as it
/// ends: each measured file's, linked to the points, decisions and arm
/// selections of `manifest` and to the value probes the run placed
/// (`value_sites`, `rust_value_probes`).
pub(crate) fn list_rust_run(
    root: &std::path::Path,
    manifest: &crate::coverage_report::CoverageManifest,
    value_sites: impl IntoIterator<Item = String>,
) -> MutantsFile {
    let mut recorded = BTreeMap::<String, std::collections::BTreeSet<String>>::new();
    for point in &manifest.points {
        recorded
            .entry(point.file.clone())
            .or_default()
            .insert(point.id.clone());
    }
    for decision in &manifest.decisions {
        recorded
            .entry(decision.file.clone())
            .or_default()
            .insert(decision.id.clone());
    }
    for branch in &manifest.branches {
        recorded.entry(branch.file.clone()).or_default().extend(
            branch
                .alternatives
                .iter()
                .map(|alternative| alternative.id.clone()),
        );
    }
    // A site is named by its file's digest only: every file is offered them.
    let sites = value_sites
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for ids in recorded.values_mut() {
        ids.extend(sites.iter().cloned());
    }
    recorded.retain(|file, _| file.ends_with(".rs"));
    list_rust_mutants(root, &recorded).mutants
}

pub(crate) struct ListedMutants {
    pub(crate) mutants: MutantsFile,
    pub(crate) changed: Vec<String>,
}

/// The mutants of the files in `recorded` (each file's point ids, as the
/// run's manifest has them), read from under `root`, each file by its
/// language's enumerator. A language with no enumerator that lists after a
/// run lists none of its files.
pub(crate) fn list_from_sources(
    root: &std::path::Path,
    recorded: &BTreeMap<String, std::collections::BTreeSet<String>>,
) -> ListedMutants {
    let mut by_language =
        BTreeMap::<&str, BTreeMap<String, std::collections::BTreeSet<String>>>::new();
    for (file, points) in recorded {
        if let Some(language) = language_of_path(file) {
            by_language
                .entry(language)
                .or_default()
                .insert(file.clone(), points.clone());
        }
    }
    let mut listed = ListedMutants {
        mutants: MutantsFile::unlisted(),
        changed: Vec::new(),
    };
    for (language, files) in by_language {
        let one = match language {
            JAVASCRIPT => crate::js_mutant_links::list_mutants(root, &files),
            JVM => {
                let (files, changed) = crate::jvm_strong::list_mutants(root, &files);
                ListedMutants {
                    mutants: MutantsFile::listed(JVM, crate::jvm_mutants::MUTANT_SET, files),
                    changed,
                }
            }
            RUST => list_rust_mutants(root, &files),
            crate::go_mutants::GO => crate::go_mutants::list_mutants(root, &files),
            crate::python_mutants::PYTHON => crate::python_mutants::list_mutants(root, &files),
            crate::ruby_mutants::RUBY => crate::ruby_mutants::list_mutants(root, &files),
            _ => continue,
        };
        listed.changed.extend(one.changed);
        listed.mutants = listed
            .mutants
            .merge(one.mutants)
            .expect("each language is listed once");
    }
    listed
}

/// `mutants.json` as it is stored: each mutant without what is worked out
/// again when it is read. An `id` is left out (empty) where it is the one its
/// file, span, mutator and descriptor make, a probe or comparison site in the
/// mutant's own file is written from its line (`:L:C`, which no file name
/// starts), and a long original is cut to what is shown of it where that
/// shows exactly the same.
#[derive(Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct StoredMutants {
    schema_version: u32,
    mutant_set: Option<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    mutant_sets: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    languages: BTreeMap<String, String>,
    files: BTreeMap<String, Vec<CandidateMutant>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    tests_without_values: Vec<String>,
}

/// The id a mutant of `file` has from its span, mutator and descriptor.
fn mutant_id(file: &str, mutant: &CandidateMutant) -> String {
    format!(
        "{file}:{}:{}-{}:{}:{}:{}",
        mutant.start.0,
        mutant.start.1,
        mutant.end.0,
        mutant.end.1,
        mutant.mutator,
        mutant.replacement
    )
}

impl From<MutantsFile> for StoredMutants {
    fn from(file: MutantsFile) -> Self {
        let files = file
            .files
            .into_iter()
            .map(|(path, mutants)| {
                let stored = mutants
                    .into_iter()
                    .map(|mutant| {
                        let mut stored =
                            crate::js_weak_mutation::abbreviated(&mutant).unwrap_or(mutant);
                        if stored.id == mutant_id(&path, &stored) {
                            stored.id.clear();
                        }
                        for site in [&mut stored.compare, &mut stored.probe]
                            .into_iter()
                            .flatten()
                        {
                            if let Some(local) = site
                                .strip_prefix(path.as_str())
                                .filter(|local| local.starts_with(':'))
                            {
                                *site = local.to_owned();
                            }
                        }
                        stored
                    })
                    .collect();
                (path, stored)
            })
            .collect();
        Self {
            schema_version: MUTANTS_SCHEMA_VERSION,
            mutant_set: file.mutant_set,
            mutant_sets: file.mutant_sets,
            languages: file.languages,
            files,
            tests_without_values: file.tests_without_values,
        }
    }
}

impl From<StoredMutants> for MutantsFile {
    fn from(stored: StoredMutants) -> Self {
        let mut files = stored.files;
        for (path, mutants) in &mut files {
            for mutant in mutants.iter_mut() {
                if mutant.id.is_empty() {
                    mutant.id = mutant_id(path, mutant);
                }
                for site in [&mut mutant.compare, &mut mutant.probe]
                    .into_iter()
                    .flatten()
                {
                    if site.starts_with(':') {
                        site.insert_str(0, path);
                    }
                }
            }
        }
        Self {
            schema_version: stored.schema_version,
            mutant_set: stored.mutant_set,
            mutant_sets: stored.mutant_sets,
            languages: stored.languages,
            files,
            tests_without_values: stored.tests_without_values,
        }
    }
}

/// One mutant and the evidence that decides it: StrykerJS's in JavaScript,
/// the reference tool's in another language. The links past `function` are
/// what a language's value probes record (`docs/design/mutation-drivers.md`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CandidateMutant {
    /// `file:startLine:startCol-endLine:endCol:Mutator:descriptor`. Left
    /// out of `mutants.json` where it is exactly that.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub id: String,
    pub mutator: String,
    /// The descriptor (see `js_mutants`).
    pub replacement: String,
    /// 1-based line and 1-based UTF-16 column, as StrykerJS reports them.
    pub start: (u32, u32),
    pub end: (u32, u32),
    /// The mutated span's source text. In `mutants.json`, of a long span
    /// only what is shown of it: the start of each operand around an
    /// operator, or of the whole, with `operator.at` where it is there.
    pub original: String,
    /// The operator of a binary, logical or assignment expression mutated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operator: Option<MutantOperator>,
    /// A `ConditionalExpression` or `LogicalOperator` mutant whose value, not
    /// only its truthiness, can reach the program: the original agreeing in
    /// truthiness with the mutant does not show their values were equal.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub value_used: bool,
    /// A `ConditionalExpression` mutant whose original runs code or changes
    /// state as it is evaluated (a call in a condition): the literal that
    /// replaces it skips that, so the condition's value alone cannot show
    /// the mutant changes nothing.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub effects: bool,
    /// The innermost statement point containing the mutant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub statement: Option<String>,
    /// The innermost function point containing the mutant (an arrow's own).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub function: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<DecisionLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<BranchLink>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain: Option<ChainLink>,
    /// The comparison probe's site (`file:L:C`), when one was placed on the
    /// mutated comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compare: Option<String>,
    /// The infection probe's site (`file:L:C-EL:EC#kind`), when one was
    /// placed on the mutated expression.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe: Option<String>,
    /// What the source says of a concise arrow's body.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow: Option<ArrowLink>,
    /// The static rule that shows no input can make the mutant differ
    /// (`js_static_mutants`): its status is then `equivalent`, whatever the
    /// tests did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub equivalent: Option<StaticNote>,
    /// Why the input that would make it differ is practically out of a
    /// test's reach (`clock`, `random`): a hint that changes no status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub untestable: Option<StaticNote>,
    /// A module-level mutant inside a literal a top-level declaration makes,
    /// where nothing but reads of the literal can carry its value
    /// (`js_value_reads`): the points a read of it hits, which narrow its
    /// candidates to the tests that read it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reads: Option<ReadLink>,
    /// Strong mode (`js_schemata`): its switch is evaluated while the module
    /// loads, so a strong pass runs whole files for it.
    #[serde(default, rename = "static", skip_serializing_if = "std::ops::Not::not")]
    pub is_static: bool,
    /// Strong mode: why the mutant has no switch (`no-placeable-ancestor`,
    /// `function-source`, `with-statement`, `node-not-found`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unplaceable: Option<String>,
    /// The mutated span lies wholly inside the arguments of a logging call
    /// (`console.*`, or a method named `info`, `warn`, `error`, `debug`,
    /// `trace` or `log` on any receiver): all it can change is a log line.
    /// A classification for reports and triage; it changes no status.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub log_only: bool,
}

/// The points of a module-level value's reads (`js_value_reads`): `read`
/// is hit by a read of the mutant's place, `escape` by such a read made while
/// a module loaded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadLink {
    pub read: String,
    pub escape: String,
}

/// A rule's stable code and the sentence that says why it applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StaticNote {
    pub rule: String,
    pub reason: String,
}

/// An operator and where it is: a byte offset into the mutant's `original`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MutantOperator {
    pub text: String,
    pub at: usize,
}

/// The decision the mutant is: all of it (no index), exactly one of its
/// conditions, or exactly its first `prefix` conditions (two or more, made
/// only of `&&` and `||`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DecisionLink {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prefix: Option<usize>,
}

/// A logical selection (`a && b`, `a ?? b`, `x ||= y`) the mutant is, or is
/// an operand of.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BranchLink {
    pub id: String,
    pub role: BranchRole,
    /// `&&`, `||`, `??`, or a logical assignment's `&&=`, `||=`, `??=`.
    pub operator: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase")]
pub enum BranchRole {
    Value,
    Left,
    Right,
}

/// The optional link the mutant removes, and the optional links of the same
/// chain evaluated before it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChainLink {
    pub id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier: Vec<String>,
}

/// A concise arrow's body: whether evaluating it has effects (which the
/// mutant's `undefined` removes), and whether it can never be `undefined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ArrowLink {
    pub effects: bool,
    pub never_undefined: bool,
}

#[cfg(test)]
mod tests {
    /// A Rust run's mutants are listed from every file its manifest names
    /// through a point, a decision or a branch, and only its Rust files: a
    /// file gone since is no list, and a manifest of nothing lists nothing.
    #[test]
    fn a_rust_run_lists_the_files_its_decisions_and_branches_name() {
        use crate::coverage_report::{
            BranchAlternativeMeta, BranchMeta, CoverageManifest, DecisionMeta,
        };
        let manifest = CoverageManifest {
            decisions: vec![DecisionMeta {
                id: "d".into(),
                file: "src/gone.rs".into(),
                line: 1,
                column: 1,
                source: "a && b".into(),
                conditions: vec!["a".into(), "b".into()],
                kind: "if".into(),
            }],
            points: vec![],
            branches: vec![BranchMeta {
                id: "b".into(),
                kind: "match".into(),
                file: "web/app.js".into(),
                line: 1,
                column: 1,
                source: "x".into(),
                alternatives: vec![BranchAlternativeMeta {
                    id: "b:0".into(),
                    label: "A".into(),
                }],
            }],
            limitations: vec![],
            unmeasured: vec![],
            scope: None,
            assertion_sites: Vec::new(),
            probe_tables: Default::default(),
        };
        let root =
            std::env::temp_dir().join(format!("supercov-rust-run-list-{}", std::process::id()));
        let listed = list_rust_run(&root, &manifest, ["site".to_owned()]);
        assert!(listed.files.is_empty());
        let nothing = CoverageManifest {
            decisions: vec![],
            branches: vec![],
            ..manifest
        };
        assert!(
            list_rust_run(&root, &nothing, Vec::<String>::new())
                .files
                .is_empty()
        );
    }

    use super::*;

    fn mutant(file: &str) -> CandidateMutant {
        serde_json::from_value(serde_json::json!({
            "id": format!("{file}:1:1-1:5:BooleanLiteral:false"),
            "mutator": "BooleanLiteral", "replacement": "false",
            "start": [1, 1], "end": [1, 5], "original": "true"
        }))
        .unwrap()
    }

    #[test]
    fn a_go_file_s_mutants_are_listed_from_its_source_after_the_run() {
        let root = std::env::temp_dir().join(format!("supercov-list-go-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let source = "package p\n\nfunc f(a int) int { return a + 1 }\n";
        std::fs::write(root.join("a.go"), source).unwrap();
        let points = crate::go_instrumenter::build_go_obligations("a.go", source, &mut 0, &mut 0)
            .unwrap()
            .manifest
            .points
            .into_iter()
            .map(|point| point.id)
            .collect();
        // A language that lists nothing after a run, and a file of none.
        let recorded = BTreeMap::from([
            ("a.go".to_owned(), points),
            ("b.cob".to_owned(), Default::default()),
            ("notes.txt".to_owned(), Default::default()),
        ]);
        let listed = list_from_sources(&root, &recorded);
        assert_eq!(listed.mutants.files.keys().collect::<Vec<_>>(), ["a.go"]);
        assert!(listed.changed.is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    fn one(language: &str, set: &str, file: &str) -> MutantsFile {
        MutantsFile::listed(
            language,
            set,
            BTreeMap::from([(file.to_owned(), vec![mutant(file)])]),
        )
    }

    /// A JavaScript list is stored as every list was before languages: no
    /// language and no sets. Another language's names both, and reads back.
    #[test]
    fn a_list_names_the_languages_of_files_that_are_not_javascript() {
        let javascript = one(JAVASCRIPT, "stryker-js@10.1.1", "src/a.js");
        let stored = serde_json::to_value(&javascript).unwrap();
        assert_eq!(
            stored,
            serde_json::json!({
                "schemaVersion": 1,
                "mutantSet": "stryker-js@10.1.1",
                "files": {"src/a.js": [{
                    "mutator": "BooleanLiteral", "replacement": "false",
                    "start": [1, 1], "end": [1, 5], "original": "true"
                }]}
            })
        );
        assert_eq!(javascript.language_of("src/a.js"), JAVASCRIPT);
        let python = one("python", "mutmut@3", "src/a.py");
        let stored = serde_json::to_value(&python).unwrap();
        assert_eq!(
            stored["mutantSets"],
            serde_json::json!({"python": "mutmut@3"})
        );
        assert_eq!(
            stored["languages"],
            serde_json::json!({"src/a.py": "python"})
        );
        let read: MutantsFile = serde_json::from_value(stored).unwrap();
        assert_eq!(read, python);
        assert_eq!(read.language_of("src/a.py"), "python");
        assert_eq!(
            read.sets(),
            BTreeMap::from([("python".to_owned(), "mutmut@3".to_owned())])
        );
        assert!(MutantsFile::unlisted().sets().is_empty());
    }

    /// Lists merge file by file, each language keeping its set; a language
    /// listed with two sets does not merge.
    #[test]
    fn lists_of_several_languages_merge_and_keep_each_set() {
        let merged = one(JAVASCRIPT, "stryker-js@10.1.1", "src/a.js")
            .merge(one("python", "mutmut@3", "src/a.py"))
            .unwrap();
        assert_eq!(
            merged.mutant_set.as_deref(),
            Some("stryker-js@10.1.1, mutmut@3")
        );
        assert_eq!(merged.language_of("src/a.js"), JAVASCRIPT);
        assert_eq!(merged.language_of("src/a.py"), "python");
        assert_eq!(merged.sets().len(), 2);
        // A file both listed keeps the first list's mutants.
        let mut again = one("python", "mutmut@3", "src/a.py");
        again.files.get_mut("src/a.py").unwrap().clear();
        let kept = merged.clone().merge(again).unwrap();
        assert_eq!(kept.files["src/a.py"].len(), 1);
        // JavaScript merged with JavaScript names no sets.
        let both = one(JAVASCRIPT, "s", "src/a.js")
            .merge(one(JAVASCRIPT, "s", "src/b.js"))
            .unwrap();
        assert!(both.mutant_sets.is_empty());
        assert_eq!(both.files.len(), 2);
        // An unlisted list adds nothing, and takes the other whole.
        assert_eq!(
            merged.clone().merge(MutantsFile::unlisted()).unwrap(),
            merged
        );
        assert_eq!(
            MutantsFile::unlisted().merge(merged.clone()).unwrap(),
            merged
        );
        assert_eq!(
            MutantsFile::unlisted()
                .merge(MutantsFile::unlisted())
                .unwrap(),
            MutantsFile::unlisted()
        );
        assert_eq!(
            merged
                .merge(one("python", "mutmut@4", "src/b.py"))
                .unwrap_err(),
            "its python mutants are those of mutmut@4, and the other's those of mutmut@3"
        );
    }

    /// Files measured without their mutants go to their language's
    /// enumerator; a language without one, or a file of none, lists
    /// nothing.
    #[test]
    fn files_measured_without_their_mutants_are_listed_by_their_language() {
        let root = std::env::temp_dir().join(format!(
            "supercov-mutant-list-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.js"), "export const a = (x) => x > 1;\n").unwrap();
        std::fs::write(root.join("src/a.py"), "a = 1 > 2\n").unwrap();
        std::fs::write(
            root.join("src/a.rs"),
            "pub fn a(x: u32) -> bool {\n    x > 1\n}\n",
        )
        .unwrap();
        std::fs::write(root.join("notes.txt"), "a\n").unwrap();
        std::fs::write(root.join("src/empty.rs"), "// nothing to mutate\n").unwrap();
        std::fs::write(root.join("src/a.cob"), "DISPLAY 'A'.\n").unwrap();
        std::fs::write(root.join("src/A.java"), "class A {}\n").unwrap();
        let recorded = [
            "src/a.js",
            "src/a.py",
            "src/a.rs",
            "src/a.cob",
            "src/A.java",
            "src/empty.rs",
            "src/gone.rs",
            "notes.txt",
        ]
        .into_iter()
        .map(|file| (file.to_owned(), std::collections::BTreeSet::new()))
        .collect::<BTreeMap<_, _>>();
        // The JavaScript and Python files went to their enumerators, which
        // say neither is the file the run measured (no point of it was
        // recorded); a file of a language with no enumerator (COBOL, and
        // Java, which lists no mutants yet), and the text file, to none.
        let listed = list_from_sources(&root, &recorded);
        // A Rust file that is gone changed; one with no mutant lists none.
        assert_eq!(listed.changed, ["src/a.js", "src/a.py", "src/gone.rs"]);
        // cargo-mutants' mutants of the Rust file, under its set.
        assert_eq!(
            listed.mutants.files.keys().collect::<Vec<_>>(),
            ["src/a.rs"]
        );
        assert_eq!(listed.mutants.language_of("src/a.rs"), RUST);
        // A crate in a directory of its own, its points named by its
        // files' paths there.
        std::fs::create_dir_all(root.join("web/rust/src")).unwrap();
        std::fs::write(root.join("web/rust/Cargo.toml"), "[package]\n").unwrap();
        let source = "pub fn a(x: u32) -> bool {\n    x > 1\n}\n";
        std::fs::write(root.join("web/rust/src/lib.rs"), source).unwrap();
        let points = crate::rust_instrumenter::build_rust_manifest("src/lib.rs", source)
            .unwrap()
            .points
            .into_iter()
            .map(|point| point.id)
            .collect::<std::collections::BTreeSet<_>>();
        let nested = list_from_sources(
            &root,
            &BTreeMap::from([("web/rust/src/lib.rs".to_owned(), points.clone())]),
        );
        assert!(nested.changed.is_empty());
        assert!(!nested.mutants.files["web/rust/src/lib.rs"].is_empty());
        // Points of another file are a changed file, wherever it is.
        let mut other = points;
        other.insert("rs:statement:elsewhere".into());
        let changed = list_from_sources(
            &root,
            &BTreeMap::from([("web/rust/src/lib.rs".to_owned(), other)]),
        );
        assert_eq!(changed.changed, ["web/rust/src/lib.rs"]);
        let nothing = list_from_sources(&root, &BTreeMap::new());
        assert_eq!(nothing.mutants, MutantsFile::unlisted());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_file_s_extension_names_its_language() {
        for (path, language) in [
            ("a.ts", Some(JAVASCRIPT)),
            ("a.py", Some("python")),
            ("a.rs", Some("rust")),
            ("a.rb", Some("ruby")),
            ("a.go", Some("go")),
            ("A.java", Some("jvm")),
            ("a.txt", None),
            ("Makefile", None),
        ] {
            assert_eq!(language_of_path(path), language, "{path}");
        }
    }
}
