//! Whether a run is still a record of a checkout whose test-side files have
//! moved on, judged file by file.
//!
//! A run is fingerprinted by domain -- source, tests, dependencies,
//! configuration -- and any change in a domain made it out of date. For the
//! source and the dependencies that is right. For the rest it was too blunt:
//! the end-to-end suite's Playwright configuration or helpers changing made
//! every unit run out of date, and a helper that only gained a function for a
//! new test made every run that loads it one. Each file that differs is
//! judged here by what it is and what the run's tests did with it.
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde_json::Value;

use crate::{
    evidence_archive::{EvidenceArchiveEntry, read_archive_selected},
    run_store::StoredRun,
    source_manifest::{FileFingerprint, FileManifest, describe},
    source_units::only_added,
    test_loads::{Imports, configuration_runner, reads_imports},
};

/// The archive entry holding the Playwright configuration a run resolved.
pub const PLAYWRIGHT_CONFIGURATION: &str = "playwright-configuration.json";

/// What a run's archive says about how it ran, beyond its files.
#[derive(Debug, Clone, Default)]
pub struct Ran {
    /// The runners its tests ran under, where its archive says.
    pub runners: Option<BTreeSet<String>>,
    /// The Playwright configuration as it resolved, where it ran Playwright.
    pub playwright: Option<Value>,
    /// The files it measured, from its coverage manifest's scope. A file of
    /// the source outside them -- a simulator, a script -- is judged as a
    /// helper is.
    pub measured: Option<BTreeSet<String>>,
    /// Which of its tests reached which files through the processes that
    /// served them (support_reach.rs), where it recorded that.
    pub support: Option<crate::support_reach::SupportReach>,
}

impl Ran {
    pub fn of(run: &StoredRun) -> Self {
        let merged = run.metadata.merged == Some(true);
        match read_archive_selected(&run.evidence_path, |path| {
            Self::wanted(path, merged).is_some()
        }) {
            Ok(entries) => Self::from_entries(&entries, merged),
            Err(_) => Self::default(),
        }
    }

    /// The same, from an archive already read.
    pub fn from_entries(entries: &[EvidenceArchiveEntry], merged: bool) -> Self {
        let mut ran = Self::default();
        for entry in entries {
            let Some(name) = Self::wanted(&entry.path, merged) else {
                continue;
            };
            let Ok(value) = serde_json::from_slice::<Value>(&entry.contents) else {
                continue;
            };
            if name == crate::support_reach::ARCHIVE_PATH {
                if let Ok(reach) =
                    serde_json::from_value::<crate::support_reach::SupportReach>(value)
                {
                    ran.support
                        .get_or_insert_with(Default::default)
                        .extend(reach);
                }
            } else if name == PLAYWRIGHT_CONFIGURATION {
                ran.playwright = Some(value);
            } else if name == "manifest.json" {
                ran.measured = value["scope"]["entries"].as_array().map(|entries| {
                    entries
                        .iter()
                        .filter(|entry| entry["status"] == "included")
                        .filter_map(|entry| entry["file"].as_str().map(str::to_owned))
                        .collect()
                });
            } else if let Some(runners) = value["runners"].as_array() {
                ran.runners.get_or_insert_with(BTreeSet::new).extend(
                    runners
                        .iter()
                        .filter_map(|runner| runner["runner"].as_str())
                        .filter(|runner| !matches!(*runner, "command" | "background"))
                        .map(str::to_owned),
                );
            }
        }
        ran
    }

    /// Which of the two entries a path is, in a run or a merge of runs.
    fn wanted(path: &str, merged: bool) -> Option<&'static str> {
        let name = if merged {
            path.rsplit('/').next().unwrap_or_default()
        } else {
            path
        };
        [
            PLAYWRIGHT_CONFIGURATION,
            "frontend.json",
            "manifest.json",
            crate::support_reach::ARCHIVE_PATH,
        ]
        .into_iter()
        .find(|wanted| *wanted == name)
    }
}

/// The verdict on one run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Judgment {
    /// Files whose change makes the run a record of something else, each with
    /// what moved in it.
    pub outdated: Vec<String>,
    /// Whether one of those is a file of the run's own tests: no option makes
    /// a changed test's record its own.
    pub own_tests: bool,
    /// Whether one of those is one no option accepts: a file of its own
    /// tests, or one it measured.
    pub settled: bool,
    /// Files that differ and do not reach the run, with why; and those the
    /// user accepted.
    pub across: Vec<String>,
    /// Files that changed and that only some of its tests reached, through
    /// a process that served them: those tests are a record of something
    /// else, and the rest still of what they ran (support_reach.rs).
    pub reached: BTreeMap<String, BTreeSet<String>>,
}

/// One checkout's files, read once for every run judged against it.
pub struct Checkout<'a> {
    pub root: &'a Path,
    /// Every file known to hold tests, from every run there is.
    pub tests: &'a BTreeSet<String>,
    imports: Imports,
    /// What all the known test files load: a helper none of them loads is one
    /// Supercov cannot place, so it counts against every run. Read when a
    /// helper first needs placing.
    loaded_by_tests: std::cell::OnceCell<crate::test_loads::Loads>,
}

impl<'a> Checkout<'a> {
    pub fn new(root: &'a Path, tests: &'a BTreeSet<String>) -> Self {
        Self {
            root,
            tests,
            imports: Imports::new(root),
            loaded_by_tests: std::cell::OnceCell::new(),
        }
    }

    /// Whether some known test file imports `file`: one that none does is
    /// loaded from where Supercov cannot see -- a pattern in a configuration,
    /// a command -- and may be loaded by any test. A run whose own imports
    /// are all followed and miss it does not load it.
    pub fn places(&self, file: &str) -> bool {
        self.loaded_by_tests
            .get_or_init(|| {
                self.imports.loads(
                    self.tests
                        .iter()
                        .filter(|file| reads_imports(file))
                        .map(String::as_str),
                )
            })
            .files
            .contains(file)
    }

    /// What a run's tests load: their files, and the configuration of each
    /// runner they ran under with what it names. `None` where a file of
    /// theirs is not one Supercov reads imports from.
    pub fn loads(
        &self,
        own: &BTreeSet<String>,
        runners: Option<&BTreeSet<String>>,
        configurations: &[String],
    ) -> Option<crate::test_loads::Loads> {
        if own.is_empty() || !own.iter().all(|file| reads_imports(file)) {
            return None;
        }
        let mut entries = own.iter().cloned().collect::<Vec<_>>();
        for configuration in configurations {
            let used = configuration_runner(configuration)
                .is_some_and(|runner| runners.is_some_and(|runners| runners.contains(runner)));
            if used {
                entries.push(configuration.clone());
                entries.extend(self.imports.named_by(configuration));
            }
        }
        Some(self.imports.loads(entries.iter().map(String::as_str)))
    }
}

/// A file that sets how tests or builds run, by the rule the fingerprint's
/// configuration domain uses.
pub fn configuration_file(file: &str) -> bool {
    crate::integrity::configuration_file(Path::new(file))
}

/// The configuration files in the checkout now, relative to `root`, found as
/// the fingerprint finds them; none when the checkout cannot be walked.
pub fn configuration_files_in(root: &Path) -> Vec<String> {
    crate::integrity::walked_configuration_files(root)
        .unwrap_or_default()
        .iter()
        .filter_map(|file| file.strip_prefix(root).ok())
        .map(|file| file.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// A file that sits with the tests by its path, as the fingerprint's test
/// domain finds them: in a `test`, `tests` or `__tests__` directory, or named
/// for tests.
pub fn beside_the_tests(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    lower
        .split('/')
        .next()
        .is_some_and(|top| matches!(top, "test" | "tests" | "__tests__"))
        || lower
            .split(['/', '_', '.', '-'])
            .any(|part| matches!(part, "test" | "spec"))
}

pub fn dependency_file(file: &str) -> bool {
    let name = file.rsplit('/').next().unwrap_or(file);
    matches!(
        name,
        "package.json"
            | "package-lock.json"
            | "npm-shrinkwrap.json"
            | "pnpm-lock.yaml"
            | "yarn.lock"
            | "bun.lock"
            | "bun.lockb"
    )
}

/// The Playwright settings that decide what a test does, as opposed to when
/// and how often it runs: the rest -- timeouts, retries, workers, reporters,
/// which files and names to run -- schedules tests and changes nothing a test
/// that ran reaches.
fn playwright_shape(configuration: &Value) -> Value {
    const PROJECT: [&str; 5] = ["name", "use", "testDir", "dependencies", "teardown"];
    // At the top level `use` and `testDir` are what every project inherits.
    const CONFIG: [&str; 5] = [
        "globalSetup",
        "globalTeardown",
        "webServer",
        "use",
        "testDir",
    ];
    let pick = |value: &Value, keys: &[&str]| {
        Value::Object(
            keys.iter()
                .filter_map(|key| value.get(*key).map(|v| ((*key).to_owned(), v.clone())))
                .collect(),
        )
    };
    let mut shape = pick(configuration, &CONFIG);
    shape["projects"] = Value::Array(
        configuration["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|project| pick(project, &PROJECT))
            .collect(),
    );
    shape
}

/// Whether the run's Playwright configuration and the reference's do the same
/// to the run's tests: every project it had is there and set alike, and the
/// servers and global setup are the same. A project added since runs none of
/// its tests. Where they differ, which settings did.
pub fn playwright_differences(run: &Value, reference: &Value) -> Vec<String> {
    let (run, reference) = (playwright_shape(run), playwright_shape(reference));
    let mut differences = Vec::new();
    for key in [
        "globalSetup",
        "globalTeardown",
        "webServer",
        "use",
        "testDir",
    ] {
        if run.get(key) != reference.get(key) {
            differences.push(key.to_owned());
        }
    }
    let projects = |shape: &Value| {
        shape["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|project| {
                (
                    project["name"].as_str().unwrap_or("").to_owned(),
                    project.clone(),
                )
            })
            .collect::<BTreeMap<_, _>>()
    };
    let now = projects(&reference);
    for (name, project) in projects(&run) {
        let label = if name.is_empty() {
            String::new()
        } else {
            format!("project {name}: ")
        };
        match now.get(&name) {
            None => differences.push(format!("{label}removed")),
            Some(current) => {
                for key in ["use", "testDir", "dependencies", "teardown"] {
                    if project.get(key) != current.get(key) {
                        differences.push(format!("{label}{key}"));
                    }
                }
            }
        }
    }
    differences
}

/// What the files a run was recorded with say against `now`, the files of the
/// checkout or of the later run it is merged into.
///
/// `text` gives a file's current text where it is the text `now` describes;
/// `reference` is how Playwright resolved for the later run, where one did;
/// `label` names the run in what is said about it.
#[allow(clippy::too_many_arguments)]
pub fn judge(
    checkout: &Checkout<'_>,
    label: &str,
    recorded: &FileManifest,
    now: &FileManifest,
    text: &dyn Fn(&str) -> Option<String>,
    own: Option<&BTreeSet<String>>,
    ran: &Ran,
    reference: Option<&Value>,
    allow_changed: bool,
) -> Judgment {
    let mut judgment = Judgment::default();
    let configurations = recorded
        .keys()
        .chain(now.keys())
        .filter(|file| configuration_file(file))
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let loads = own.and_then(|own| checkout.loads(own, ran.runners.as_ref(), &configurations));
    let mut changed = recorded
        .iter()
        .filter(|(file, was)| now.get(*file).is_none_or(|is| !is.same_bytes(was)))
        .map(|(file, _)| file.clone())
        .collect::<BTreeSet<_>>();
    // A configuration that did not exist then can change a runner too.
    changed.extend(
        now.keys()
            .filter(|file| !recorded.contains_key(*file) && configuration_file(file))
            .cloned(),
    );
    let refuse = |judgment: &mut Judgment, file: &str, what: String| {
        if allow_changed {
            judgment.across.push(format!("{file} (accepted)"));
        } else {
            judgment.outdated.push(what);
        }
    };
    for file in changed {
        let gone = !now.contains_key(&file);
        if own.is_some_and(|own| own.contains(&file)) {
            judgment.own_tests = true;
            judgment.settled = true;
            judgment.outdated.push(format!(
                "{file}, a file of its own tests, {}",
                if gone { "is gone" } else { "has changed" }
            ));
            continue;
        }
        // Another run's test: this run did not run it. Where the run does not
        // say which files are its own, every test file counts.
        if checkout.tests.contains(&file) && own.is_some() {
            continue;
        }
        if dependency_file(&file) {
            judgment.outdated.push(format!("{file} has changed"));
            continue;
        }
        if configuration_file(&file) {
            let runner = configuration_runner(&file);
            match (runner, ran.runners.as_ref()) {
                (Some(runner), Some(runners)) if !runners.contains(runner) => {
                    judgment.across.push(format!(
                        "{file} ({runner} settings; `{label}` ran under {})",
                        runners.iter().cloned().collect::<Vec<_>>().join(", ")
                    ));
                }
                (Some("playwright"), Some(_)) => match (&ran.playwright, reference) {
                    (Some(then), Some(reference)) => {
                        let differences = playwright_differences(then, reference);
                        if differences.is_empty() {
                            judgment
                                .across
                                .push(format!("{file} (only settings that schedule tests)"));
                        } else {
                            refuse(
                                &mut judgment,
                                &file,
                                format!("{file}: {} changed", differences.join(", ")),
                            );
                        }
                    }
                    _ => refuse(&mut judgment, &file, format!("{file} has changed")),
                },
                _ => refuse(&mut judgment, &file, format!("{file} has changed")),
            }
            continue;
        }
        // What the run measured has moved on: its numbers are of other code.
        // Where the run does not say what it measured, any file not beside
        // the tests may be.
        let measured = match &ran.measured {
            Some(measured) => measured.contains(&file),
            None => !beside_the_tests(&file),
        };
        if measured {
            judgment.settled = true;
            judgment.outdated.push(format!(
                "{file}, which it measured, {}",
                if gone { "is gone" } else { "has changed" }
            ));
            continue;
        }
        // A file a process that served the run's tests loaded or read -- a
        // mock server's -- reached those tests and no others. A file no such
        // process loaded is judged by what the tests import, as before.
        if let Some(tests) = ran
            .support
            .as_ref()
            .and_then(|support| support.files.get(&file))
            && !loads
                .as_ref()
                .is_some_and(|loads| loads.files.contains(&file))
        {
            if tests.is_empty() {
                judgment.across.push(format!(
                    "{file} (no test of `{label}` was served by what loaded it)"
                ));
            } else {
                judgment.reached.insert(file, tests.clone());
            }
            continue;
        }
        // A file tests may load: a helper, a fixture, a support module, or
        // source outside what is measured.
        let placed = checkout.places(&file);
        if let Some(loads) = &loads
            && loads.open.is_none()
            && placed
            && !loads.files.contains(&file)
        {
            judgment
                .across
                .push(format!("{file} (`{label}` does not load it)"));
            continue;
        }
        let Some(is) = now.get(&file) else {
            refuse(&mut judgment, &file, format!("{file} is gone"));
            continue;
        };
        let before = recorded.get(&file).and_then(|was| was.code.as_ref());
        let after = is.code.as_ref();
        // Read as the checkout holds it, where that is what `now` recorded.
        let source = text(&file).filter(|source| FileFingerprint::of(source).sha256 == is.sha256);
        if let (Some(before), Some(source)) = (before, source)
            && let Some(added) = only_added(before, &file, &source)
        {
            judgment
                .across
                .push(format!("{file} (only added {})", added.join(", ")));
            continue;
        }
        let what = match (before, after) {
            (Some(before), Some(after)) => {
                format!(
                    "{file} changed: {}",
                    describe(before, after, &before.diff(after))
                )
            }
            _ => format!("{file} has changed"),
        };
        refuse(&mut judgment, &file, what);
    }
    judgment
}

/// What several judgments let through, one entry per file: `file (why;
/// why)`.
pub fn across(items: impl IntoIterator<Item = String>) -> Vec<String> {
    let mut files = BTreeMap::<String, Vec<String>>::new();
    for item in items {
        let (file, why) = match item.split_once(" (") {
            Some((file, why)) => (file.to_owned(), why.trim_end_matches(')').to_owned()),
            None => (item, String::new()),
        };
        let whys = files.entry(file).or_default();
        if !why.is_empty() && !whys.contains(&why) {
            whys.push(why);
        }
    }
    files
        .into_iter()
        .map(|(file, whys)| {
            if whys.is_empty() {
                file
            } else {
                format!("{file} ({})", whys.join("; "))
            }
        })
        .collect()
}

/// The checkout's own fingerprints of the files a run was recorded with, and
/// of every configuration file there is now.
pub fn checkout_files(
    root: &Path,
    recorded: &FileManifest,
    configurations: &[String],
) -> FileManifest {
    recorded
        .keys()
        .chain(configurations)
        .filter_map(|file| {
            let text = crate::fault_fs::read_to_string(root.join(file)).ok()?;
            Some((file.clone(), FileFingerprint::read(file, &text)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_sits_with_the_tests_by_its_directory_or_its_name() {
        for (file, beside) in [
            ("test/helpers.js", true),
            ("Tests/fixture.json", true),
            ("__tests__/setup.ts", true),
            ("src/cart.test.js", true),
            ("src/cart_spec.rb", true),
            ("spec-helpers/run.js", true),
            ("src/testing/latest.js", false),
            ("src/contest.js", false),
            ("lib/inspect.js", false),
        ] {
            assert_eq!(beside_the_tests(file), beside, "{file}");
        }
        assert!(dependency_file("packages/a/package.json"));
        assert!(dependency_file("yarn.lock"));
        assert!(!dependency_file("src/package.ts"));
    }
    use serde_json::json;

    /// What a run's archive says it ran, read from its own entries or, in a
    /// merge, each shard's: a support record of another shape, a declaration
    /// that names no runners and an entry that is not JSON say nothing.
    #[test]
    fn an_archive_of_nothing_says_nothing_of_what_ran() {
        let ran = Ran::from_entries(&[], false);
        assert!(ran.runners.is_none() && ran.playwright.is_none());
        assert!(ran.measured.is_none() && ran.support.is_none());
    }

    #[test]
    fn what_a_run_ran_is_read_from_its_entries_or_its_shards() {
        let entry = |path: &str, contents: Value| EvidenceArchiveEntry {
            path: path.into(),
            contents: serde_json::to_vec(&contents).unwrap(),
        };
        let entries = [
            entry(
                "shards/0/frontend.json",
                json!({"runners": [{"runner": "vitest"}, {"runner": "background"}]}),
            ),
            entry("shards/1/frontend.json", json!({"language": "javascript"})),
            entry("shards/1/support-reach.json", json!(["not", "a", "reach"])),
            entry(
                "shards/1/manifest.json",
                json!({"scope": {"entries": [
                    {"file": "src/a.js", "status": "included"},
                    {"file": "test/a.js", "status": "excluded"}
                ]}}),
            ),
            EvidenceArchiveEntry {
                path: "shards/2/playwright-configuration.json".into(),
                contents: b"not json".to_vec(),
            },
        ];
        let merged = Ran::from_entries(&entries, true);
        assert_eq!(merged.runners, Some(BTreeSet::from(["vitest".to_owned()])));
        assert_eq!(
            merged.measured,
            Some(BTreeSet::from(["src/a.js".to_owned()]))
        );
        assert!(merged.support.is_none());
        assert!(merged.playwright.is_none());
        // Read as one run's, a shard's entries are none of its own.
        let single = Ran::from_entries(&entries, false);
        assert!(single.runners.is_none() && single.measured.is_none());
    }

    /// A run whose tests include a file Supercov reads no imports from, or
    /// that names no test file, has no answer to what its tests load.
    #[test]
    fn what_tests_load_is_unknown_past_a_file_whose_imports_are_not_read() {
        let tests = BTreeSet::new();
        let checkout = Checkout::new(Path::new("/nonexistent"), &tests);
        let own = |files: &[&str]| files.iter().map(|file| (*file).to_owned()).collect();
        assert!(checkout.loads(&own(&[]), None, &[]).is_none());
        assert!(
            checkout
                .loads(&own(&["test/a.test.js", "test/data.json"]), None, &[])
                .is_none()
        );
        assert!(
            checkout
                .loads(&own(&["test/a.test.js"]), None, &[])
                .is_some()
        );
    }

    #[test]
    fn an_unnamed_playwright_project_that_is_gone_is_said_to_be_removed() {
        let then = json!({"projects": [{"use": {"baseURL": "http://127.0.0.1:3000"}}]});
        let now = json!({"projects": [{"name": "chromium", "use": {}}]});
        assert_eq!(playwright_differences(&then, &now), ["removed"]);
    }

    #[test]
    fn scheduling_settings_do_not_change_what_a_test_reaches() {
        let then = json!({
            "webServer": [{"command": "npm start", "url": "http://127.0.0.1:3000"}],
            "projects": [{"name": "chromium", "use": {"baseURL": "http://127.0.0.1:3000"}, "timeout": 30000, "retries": 0, "testMatch": "*.spec.ts"}],
            "workers": 1,
        });
        let mut later = then.clone();
        later["workers"] = json!(4);
        later["projects"][0]["timeout"] = json!(45000);
        later["projects"][0]["testMatch"] = json!(["*.spec.ts", "new/*.spec.ts"]);
        later["projects"]
            .as_array_mut()
            .unwrap()
            .push(json!({"name": "firefox", "use": {"browserName": "firefox"}}));
        assert_eq!(playwright_differences(&then, &later), Vec::<String>::new());

        let mut shaped = then.clone();
        shaped["projects"][0]["use"]["extraHTTPHeaders"] = json!({"x-suite": "e2e"});
        shaped["webServer"][0]["command"] = json!("npm run start:mock");
        assert_eq!(
            playwright_differences(&then, &shaped),
            ["webServer", "project chromium: use"]
        );
        // A configuration without projects sets `use` for all of them.
        let single = json!({"use": {"baseURL": "http://127.0.0.1:3000"}, "timeout": 30000});
        let mut longer = single.clone();
        longer["timeout"] = json!(45000);
        assert_eq!(
            playwright_differences(&single, &longer),
            Vec::<String>::new()
        );
        longer["use"]["locale"] = json!("de-DE");
        assert_eq!(playwright_differences(&single, &longer), ["use"]);
    }

    /// Each kind of file that differs, alone, against a checkout where one
    /// test file is the run's own and another is another run's.
    #[test]
    fn each_file_that_differs_is_judged_by_what_it_is() {
        use crate::source_manifest::FileFingerprint;
        static SEQUENCE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-run-currency-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        let helpers = "export function help() {\n  return 1;\n}\n";
        for (file, text) in [
            (
                "test/own.test.js",
                "import { help } from \"./helpers.js\";\nhelp();\n",
            ),
            (
                "test/other.test.js",
                "import { util } from \"./util.js\";\nutil();\n",
            ),
            ("test/helpers.js", helpers),
            ("test/util.js", "export function util() {}\n"),
            (
                "test/open.test.js",
                "const helper = await import(process.env.HELPER);\nhelper();\n",
            ),
        ] {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let tests = [
            "test/own.test.js",
            "test/other.test.js",
            "test/open.test.js",
        ]
        .map(str::to_owned)
        .into_iter()
        .collect::<BTreeSet<_>>();
        let checkout = Checkout::new(&root, &tests);
        let own = ["test/own.test.js".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let manifest = |files: &[(&str, &str)]| {
            files
                .iter()
                .map(|(file, text)| (file.to_string(), FileFingerprint::read(file, text)))
                .collect::<FileManifest>()
        };
        let measured_nothing = Ran {
            measured: Some(BTreeSet::new()),
            ..Ran::default()
        };
        let judge_one = |then: &[(&str, &str)],
                         now: &[(&str, &str)],
                         own: Option<&BTreeSet<String>>,
                         ran: &Ran,
                         reference: Option<&Value>,
                         allow: bool| {
            let now_texts = now
                .iter()
                .map(|(file, text)| (file.to_string(), text.to_string()))
                .collect::<BTreeMap<_, _>>();
            judge(
                &checkout,
                "run",
                &manifest(then),
                &manifest(now),
                &|file| now_texts.get(file).cloned(),
                own,
                ran,
                reference,
                allow,
            )
        };
        let then_pw = serde_json::json!({"use": {"baseURL": "http://127.0.0.1:3000"}});
        let other_pw = serde_json::json!({"use": {"baseURL": "http://127.0.0.1:4000"}});
        let playwright = Ran {
            runners: Some(["playwright".to_owned()].into_iter().collect()),
            playwright: Some(then_pw.clone()),
            ..Ran::default()
        };
        let served = |tests: &[&str]| Ran {
            measured: Some(BTreeSet::new()),
            support: Some(crate::support_reach::SupportReach {
                files: [(
                    "mocks/server.js".to_owned(),
                    tests.iter().map(|test| test.to_string()).collect(),
                )]
                .into_iter()
                .collect(),
                names: BTreeMap::new(),
            }),
            ..Ran::default()
        };
        let unit = Ran {
            runners: Some(["node:test".to_owned()].into_iter().collect()),
            ..Ran::default()
        };
        let open = ["test/open.test.js".to_owned()]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let served_helpers = Ran {
            measured: Some(BTreeSet::new()),
            support: Some(crate::support_reach::SupportReach {
                files: [("test/helpers.js".to_owned(), ["t1".to_owned()].into())]
                    .into_iter()
                    .collect(),
                names: BTreeMap::new(),
            }),
            ..Ran::default()
        };
        let cases: Vec<(&str, Judgment)> = vec![
            (
                "own changed",
                judge_one(
                    &[("test/own.test.js", "a")],
                    &[("test/own.test.js", "b")],
                    Some(&own),
                    &Ran::default(),
                    None,
                    false,
                ),
            ),
            (
                "own gone",
                judge_one(
                    &[("test/own.test.js", "a")],
                    &[],
                    Some(&own),
                    &Ran::default(),
                    None,
                    false,
                ),
            ),
            (
                "other test",
                judge_one(
                    &[("test/other.test.js", "a")],
                    &[("test/other.test.js", "b")],
                    Some(&own),
                    &Ran::default(),
                    None,
                    false,
                ),
            ),
            (
                "other test, no own",
                judge_one(
                    &[("test/other.test.js", "a")],
                    &[("test/other.test.js", "b")],
                    None,
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "dependency",
                judge_one(
                    &[("package.json", "{}")],
                    &[("package.json", "{\"a\":1}")],
                    Some(&own),
                    &Ran::default(),
                    None,
                    false,
                ),
            ),
            (
                "new configuration",
                judge_one(
                    &[],
                    &[("vitest.config.js", "export default {}")],
                    Some(&own),
                    &unit,
                    None,
                    false,
                ),
            ),
            (
                "playwright same",
                judge_one(
                    &[("playwright.config.ts", "a")],
                    &[("playwright.config.ts", "b")],
                    Some(&own),
                    &playwright,
                    Some(&then_pw),
                    false,
                ),
            ),
            (
                "playwright differs",
                judge_one(
                    &[("playwright.config.ts", "a")],
                    &[("playwright.config.ts", "b")],
                    Some(&own),
                    &playwright,
                    Some(&other_pw),
                    false,
                ),
            ),
            (
                "playwright differs, accepted",
                judge_one(
                    &[("playwright.config.ts", "a")],
                    &[("playwright.config.ts", "b")],
                    Some(&own),
                    &playwright,
                    Some(&other_pw),
                    true,
                ),
            ),
            (
                "playwright, no reference",
                judge_one(
                    &[("playwright.config.ts", "a")],
                    &[("playwright.config.ts", "b")],
                    Some(&own),
                    &playwright,
                    None,
                    false,
                ),
            ),
            (
                "jest, runners unknown",
                judge_one(
                    &[("jest.config.js", "a")],
                    &[("jest.config.js", "b")],
                    Some(&own),
                    &Ran::default(),
                    None,
                    false,
                ),
            ),
            (
                "measured gone",
                judge_one(
                    &[("src/cart.js", "a")],
                    &[],
                    Some(&own),
                    &Ran {
                        measured: Some(["src/cart.js".to_owned()].into_iter().collect()),
                        ..Ran::default()
                    },
                    None,
                    false,
                ),
            ),
            (
                "source, scope unknown",
                judge_one(
                    &[("src/cart.js", "a")],
                    &[("src/cart.js", "b")],
                    Some(&own),
                    &Ran::default(),
                    None,
                    false,
                ),
            ),
            (
                "served no test",
                judge_one(
                    &[("mocks/server.js", "a")],
                    &[("mocks/server.js", "b")],
                    Some(&own),
                    &served(&[]),
                    None,
                    false,
                ),
            ),
            (
                "served a test",
                judge_one(
                    &[("mocks/server.js", "a")],
                    &[("mocks/server.js", "b")],
                    Some(&own),
                    &served(&["t1"]),
                    None,
                    false,
                ),
            ),
            (
                "helper not loaded",
                judge_one(
                    &[("test/util.js", "a")],
                    &[("test/util.js", "b")],
                    Some(&own),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "helper, own empty",
                judge_one(
                    &[("test/util.js", "a")],
                    &[("test/util.js", "b")],
                    Some(&BTreeSet::new()),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "helper gone",
                judge_one(
                    &[("test/helpers.js", helpers)],
                    &[],
                    Some(&own),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "helper added to",
                judge_one(
                    &[("test/helpers.js", helpers)],
                    &[(
                        "test/helpers.js",
                        &format!("{helpers}export function more() {{}}\n"),
                    )],
                    Some(&own),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "helper body",
                judge_one(
                    &[("test/helpers.js", helpers)],
                    &[("test/helpers.js", &helpers.replace("1", "2"))],
                    Some(&own),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "helper, own imports cannot all be followed",
                judge_one(
                    &[("test/util.js", "a")],
                    &[("test/util.js", "b")],
                    Some(&open),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "source no test places",
                judge_one(
                    &[("lib/unused.js", "a")],
                    &[("lib/unused.js", "b")],
                    Some(&own),
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
            (
                "served and loaded",
                judge_one(
                    &[("test/helpers.js", helpers)],
                    &[("test/helpers.js", &helpers.replace("1", "2"))],
                    Some(&own),
                    &served_helpers,
                    None,
                    false,
                ),
            ),
            (
                "file without code",
                judge_one(
                    &[("test/fixture.json", "{}")],
                    &[("test/fixture.json", "[]")],
                    None,
                    &measured_nothing,
                    None,
                    false,
                ),
            ),
        ];
        // (outdated, across, reached, own tests, settled)
        type Verdict<'a> = (&'a [&'a str], &'a [&'a str], &'a [&'a str], bool, bool);
        let expected: Vec<Verdict> = vec![
            (
                &["test/own.test.js, a file of its own tests, has changed"],
                &[],
                &[],
                true,
                true,
            ),
            (
                &["test/own.test.js, a file of its own tests, is gone"],
                &[],
                &[],
                true,
                true,
            ),
            (&[], &[], &[], false, false),
            (
                &["test/other.test.js changed: top level"],
                &[],
                &[],
                false,
                false,
            ),
            (&["package.json has changed"], &[], &[], false, false),
            (
                &[],
                &["vitest.config.js (vitest settings; `run` ran under node:test)"],
                &[],
                false,
                false,
            ),
            (
                &[],
                &["playwright.config.ts (only settings that schedule tests)"],
                &[],
                false,
                false,
            ),
            (
                &["playwright.config.ts: use changed"],
                &[],
                &[],
                false,
                false,
            ),
            (&[], &["playwright.config.ts (accepted)"], &[], false, false),
            (
                &["playwright.config.ts has changed"],
                &[],
                &[],
                false,
                false,
            ),
            (&["jest.config.js has changed"], &[], &[], false, false),
            (
                &["src/cart.js, which it measured, is gone"],
                &[],
                &[],
                false,
                true,
            ),
            (
                &["src/cart.js, which it measured, has changed"],
                &[],
                &[],
                false,
                true,
            ),
            (
                &[],
                &["mocks/server.js (no test of `run` was served by what loaded it)"],
                &[],
                false,
                false,
            ),
            (&[], &[], &["mocks/server.js"], false, false),
            (
                &[],
                &["test/util.js (`run` does not load it)"],
                &[],
                false,
                false,
            ),
            (&["test/util.js changed: top level"], &[], &[], false, false),
            (&["test/helpers.js is gone"], &[], &[], false, false),
            (
                &[],
                &["test/helpers.js (only added more)"],
                &[],
                false,
                false,
            ),
            (
                &["test/helpers.js changed: help (line 1)"],
                &[],
                &[],
                false,
                false,
            ),
            (&["test/util.js changed: top level"], &[], &[], false, false),
            (
                &["lib/unused.js changed: top level"],
                &[],
                &[],
                false,
                false,
            ),
            (
                &["test/helpers.js changed: help (line 1)"],
                &[],
                &[],
                false,
                false,
            ),
            (&["test/fixture.json has changed"], &[], &[], false, false),
        ];
        assert_eq!(cases.len(), expected.len());
        for ((name, judgment), (outdated, across, reached, own_tests, settled)) in
            cases.iter().zip(&expected)
        {
            assert_eq!(judgment.outdated, *outdated, "{name}");
            assert_eq!(judgment.across, *across, "{name}");
            assert_eq!(
                judgment
                    .reached
                    .keys()
                    .map(String::as_str)
                    .collect::<Vec<_>>(),
                *reached,
                "{name}"
            );
            assert_eq!(
                (judgment.own_tests, judgment.settled),
                (*own_tests, *settled),
                "{name}"
            );
        }
        assert_eq!(
            cases[14].1.reached["mocks/server.js"],
            ["t1".to_owned()].into()
        );
    }

    #[test]
    fn what_several_judgments_let_through_is_one_entry_per_file() {
        assert_eq!(
            across([
                "b.js (accepted)".to_owned(),
                "a.js".to_owned(),
                "b.js (only added more)".to_owned(),
                "b.js (accepted)".to_owned(),
                "a.js ()".to_owned(),
            ]),
            ["a.js", "b.js (accepted; only added more)"]
        );
    }
}
