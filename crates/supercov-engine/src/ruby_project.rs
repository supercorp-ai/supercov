//! Ruby project discovery and ahead-of-run obligation preparation.
//!
//! The project runs in place: nothing here copies or rewrites sources. Rust
//! reads every in-scope `.rb` file once, builds the complete manifest and the
//! runtime probe plan, and records which files were included, excluded or
//! unparseable so the run can say exactly what its denominator covers.

use crate::fault_fs as fs;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};

use serde_json::json;

use crate::{
    coverage_report::CoverageManifest,
    integrity::ExplicitIntegrityInputs,
    ruby_instrumenter::{
        RUBY_PROBE_PLAN_VERSION, RUBY_PROBE_RECEIVER, RubyFilePlan, RubyProbePlan,
        build_ruby_obligations,
    },
    source_discovery::{SourceScope, SourceScopeEntry, SourceScopeMode, SourceScopeStatus},
};

pub const UNPARSEABLE_LIMITATION: &str = "ruby-source-unparseable";

/// Directories that never hold the project's own measured source. The list
/// follows what Ruby coverage tooling conventionally filters for Rails and
/// gem layouts (Rails `config/` holds boot and environment settings, not
/// application logic).
const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    ".supercov",
    ".bundle",
    "node_modules",
    "vendor",
    "tmp",
    "log",
    "coverage",
    "config",
    "db",
    "bin",
    "public",
    "storage",
    ".yardoc",
    "doc",
    "pkg",
];

const TEST_DIRECTORIES: &[&str] = &["spec", "test", "tests", "features"];

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RubyFiles {
    /// Relative, `/`-separated paths of measured application sources.
    pub sources: Vec<String>,
    /// Relative paths of specs, tests and other excluded `.rb` files.
    pub tests: Vec<String>,
    pub dependency_files: Vec<PathBuf>,
    pub configuration_files: Vec<PathBuf>,
    pub excluded: Vec<(String, &'static str)>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRubyProject {
    pub root: PathBuf,
    pub files: RubyFiles,
    pub manifest: CoverageManifest,
    pub plan: RubyProbePlan,
    pub unparseable: Vec<(String, String)>,
}

/// Asked only of a `.rb` file.
fn is_test_path(relative: &str) -> Option<&'static str> {
    let (directories, file_name) = relative.rsplit_once('/').unwrap_or(("", relative));
    if directories
        .split('/')
        .any(|component| TEST_DIRECTORIES.contains(&component))
    {
        return Some("inside a test directory");
    }
    if file_name.ends_with("_spec.rb") || file_name.ends_with("_test.rb") {
        return Some("test module by name");
    }
    if file_name.starts_with("test_") {
        return Some("test module by name");
    }
    if matches!(
        file_name,
        "spec_helper.rb" | "rails_helper.rb" | "test_helper.rb"
    ) {
        return Some("test helper");
    }
    None
}

fn walk(root: &Path, directory: &Path, files: &mut RubyFiles) -> Result<(), String> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| format!("{}: {error}", directory.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| error.to_string())?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| format!("Ruby project contains a non-UTF-8 path: {}", path.display()))?;
        let file_type = entry.file_type().map_err(|error| error.to_string())?;
        let relative = path
            .strip_prefix(root)
            .map_err(|_| format!("path escaped root: {}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        if file_type.is_dir() {
            if EXCLUDED_DIRECTORIES.contains(&name.as_str()) {
                files
                    .excluded
                    .push((relative, "tooling, dependency or generated directory"));
                continue;
            }
            walk(root, &path, files)?;
        } else if file_type.is_file() {
            match name.as_str() {
                "Gemfile" | "Gemfile.lock" | ".ruby-version" | ".tool-versions" => {
                    files.dependency_files.push(PathBuf::from(&relative));
                    continue;
                }
                ".rspec" | "Rakefile" | "config.ru" => {
                    files.configuration_files.push(PathBuf::from(&relative));
                    continue;
                }
                _ => {}
            }
            if name.ends_with(".gemspec") {
                files.dependency_files.push(PathBuf::from(&relative));
                continue;
            }
            if !name.ends_with(".rb") {
                continue;
            }
            match is_test_path(&relative) {
                Some(reason) => {
                    files.tests.push(relative.clone());
                    files.excluded.push((relative, reason));
                }
                None => files.sources.push(relative),
            }
        }
        // Symlinks are neither followed nor measured.
    }
    Ok(())
}

pub fn discover_ruby_files(root: &Path) -> Result<RubyFiles, String> {
    let mut files = RubyFiles::default();
    walk(root, root, &mut files)?;
    files.sources.sort();
    files.tests.sort();
    files.dependency_files.sort();
    files.configuration_files.sort();
    Ok(files)
}

pub fn prepare_ruby_project(
    root: &Path,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
) -> Result<PreparedRubyProject, String> {
    prepare(root, roots, false).map(|project| project.expect("refused when empty"))
}

/// The Ruby a browser suite's server runs, or None where the source roots
/// leave none: roots named for the suite's JavaScript mean there is no Ruby
/// server to measure, not a mistake.
pub fn prepare_ruby_server_project(
    root: &Path,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
) -> Result<Option<PreparedRubyProject>, String> {
    prepare(root, roots, true)
}

fn prepare(
    root: &Path,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
    server: bool,
) -> Result<Option<PreparedRubyProject>, String> {
    let mut files = discover_ruby_files(root)?;
    match roots {
        Some(roots) => {
            roots.narrow(&mut files.sources, &mut files.excluded, String::as_str);
            if server && files.sources.is_empty() {
                return Ok(None);
            }
            roots.refuse_if_empty(files.sources.len(), "Ruby")?;
        }
        None => crate::source_discovery::leave_out_what_lies_beside(
            &mut files.sources,
            &mut files.excluded,
        ),
    }
    if files.sources.is_empty() && files.tests.is_empty() {
        return Err(
            "no Ruby source files were found under the project root; Supercov measures .rb files outside vendor, db, bin and test directories".into(),
        );
    }
    let mut manifest = CoverageManifest {
        unmeasured: Vec::new(),
        decisions: Vec::new(),
        points: Vec::new(),
        branches: Vec::new(),
        limitations: Vec::new(),
        scope: None,
        assertion_sites: Vec::new(),
    };
    let mut plan_files = BTreeMap::<String, RubyFilePlan>::new();
    let mut probes = BTreeMap::new();
    let mut next_probe = 0u64;
    let mut limitation_ids = BTreeSet::new();
    let mut unparseable = Vec::new();
    for relative in &files.sources {
        let path = root.join(relative);
        let source = fs::read(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        match build_ruby_obligations(relative, &source, &mut next_probe) {
            Ok(obligations) => {
                manifest.points.extend(obligations.manifest.points);
                manifest.decisions.extend(obligations.manifest.decisions);
                manifest.branches.extend(obligations.manifest.branches);
                manifest.unmeasured.extend(obligations.manifest.unmeasured);
                for item in obligations.manifest.limitations {
                    let id = item
                        .get("id")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or_default()
                        .to_owned();
                    if limitation_ids.insert(id) {
                        manifest.limitations.push(item);
                    }
                }
                plan_files.insert(relative.clone(), obligations.plan);
                probes.extend(obligations.probes);
            }
            Err(error) => unparseable.push((relative.clone(), error.to_string())),
        }
    }
    // No limitation the instrumenter declares is the file-level one.
    if let Some((file, reason)) = unparseable.first() {
        manifest.limitations.push(json!({
            "id": UNPARSEABLE_LIMITATION,
            "kind": "source-scope",
            "file": file,
            "line": 1,
            "column": 0,
            "source": "",
            "reason": format!(
                "{} source file(s) could not be parsed and carry no obligations; first: {file}: {reason}",
                unparseable.len()
            )
        }));
    }
    manifest.unmeasured.sort();
    manifest.unmeasured.dedup();
    let mut entries = Vec::new();
    for file in &files.sources {
        let unparseable_file = unparseable.iter().any(|(path, _)| path == file);
        entries.push(SourceScopeEntry {
            file: file.clone(),
            status: if unparseable_file {
                SourceScopeStatus::Excluded
            } else {
                SourceScopeStatus::Included
            },
            reason: if unparseable_file {
                "could not be parsed".into()
            } else {
                "Ruby application source".into()
            },
            package_root: None,
        });
    }
    for (file, reason) in &files.excluded {
        if file.ends_with(".rb") {
            entries.push(SourceScopeEntry {
                file: file.clone(),
                status: SourceScopeStatus::Excluded,
                reason: (*reason).into(),
                package_root: None,
            });
        }
    }
    entries.sort_by(|left, right| left.file.cmp(&right.file));
    manifest.scope = Some(
        serde_json::to_value(SourceScope {
            version: 1,
            mode: if roots.is_some() {
                SourceScopeMode::Explicit
            } else {
                SourceScopeMode::Automatic
            },
            roots: match roots {
                Some(roots) => roots.local_roots().map_err(|error| error.to_string())?,
                None => vec![".".into()],
            },
            entries,
        })
        .expect("plain data serializes"),
    );
    let mut plan = RubyProbePlan {
        version: RUBY_PROBE_PLAN_VERSION,
        root: root.display().to_string(),
        receiver: RUBY_PROBE_RECEIVER.into(),
        files: plan_files,
        probes,
        probe_obligations: Vec::new(),
        ractors: false,
    };
    plan.probe_obligations = plan.probe_obligations();
    if mentions_ractors(root, &files) {
        plan.use_ractor_safe_receiver();
    }
    Ok(Some(PreparedRubyProject {
        root: root.to_owned(),
        plan,
        manifest,
        files,
        unparseable,
    }))
}

/// Whether any of the project's Ruby, its tests included, mentions Ractors:
/// code a test runs inside one is called from a Ractor wherever it lives.
fn mentions_ractors(root: &Path, files: &RubyFiles) -> bool {
    files.sources.iter().chain(&files.tests).any(|file| {
        fs::read(root.join(file))
            .is_ok_and(|source| source.windows(6).any(|word| word == b"Ractor"))
    })
}

pub fn ruby_integrity_inputs(files: &RubyFiles, command: &[String]) -> ExplicitIntegrityInputs {
    let execution_configuration = command.join("\0").into_bytes();
    ExplicitIntegrityInputs {
        source_files: files.sources.iter().map(PathBuf::from).collect(),
        test_files: files.tests.iter().map(PathBuf::from).collect(),
        dependency_files: files.dependency_files.clone(),
        configuration_files: files.configuration_files.clone(),
        execution_configuration,
    }
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_ambient_environment_is_not_part_of_run_identity() {
        // Identity is what the project is, not which shell it was run from.
        // Folding the environment in meant no two terminals, and no two
        // machines, ever agreed on the same run. What a project genuinely needs
        // from its environment it declares in the files that are hashed above.
        let files = RubyFiles::default();
        let command = ["rspec".to_owned(), "--format=progress".to_owned()];
        let inputs = ruby_integrity_inputs(&files, &command);
        assert_eq!(inputs.execution_configuration, b"rspec\0--format=progress");
    }
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;

    fn fixture(name: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-ruby-project-{}-{nonce}-{name}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, relative: &str, contents: &str) {
        let path = root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn explicit_roots(root: &Path, roots: &[&str]) -> crate::source_discovery::ExplicitSourceRoots {
        let roots = roots
            .iter()
            .map(|root| (*root).to_owned())
            .collect::<Vec<_>>();
        crate::source_discovery::ExplicitSourceRoots::resolve(root, &roots).unwrap()
    }

    #[test]
    fn benchmarks_and_examples_beside_the_gem_are_not_measured_unless_named() {
        // addressable keeps two benchmark scripts beside `lib`, hashie three:
        // nothing a test runs, and counted with the gem's own files.
        let root = fixture("beside");
        write(&root, "Gemfile", "source 'https://rubygems.org'\n");
        write(&root, "lib/app.rb", "def f(a)\n  a && 1\nend\n");
        write(&root, "lib/app/examples.rb", "def g\n  1\nend\n");
        write(&root, "benchmark/template_bench.rb", "require 'app'\n");
        write(&root, "examples/usage.rb", "require 'app'\n");

        let project = prepare_ruby_project(&root, None).unwrap();
        assert_eq!(
            project.files.sources,
            ["lib/app.rb", "lib/app/examples.rb"],
            "{:?}",
            project.files
        );
        assert!(
            project
                .files
                .excluded
                .contains(&("benchmark/template_bench.rb".into(), "benchmark")),
            "{:?}",
            project.files.excluded
        );
        let named =
            prepare_ruby_project(&root, Some(&explicit_roots(&root, &["examples"]))).unwrap();
        assert_eq!(named.files.sources, ["examples/usage.rb"]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn roots_named_for_a_browser_suites_javascript_mean_no_ruby_server() {
        let root = fixture("roots-name-javascript");
        write(&root, "src/app.js", "export const x = 1;\n");
        write(&root, "app.rb", "def f\n  1\nend\n");
        let roots = explicit_roots(&root, &["src"]);
        assert!(
            prepare_ruby_server_project(&root, Some(&roots))
                .unwrap()
                .is_none()
        );
        // Asked to measure Ruby itself, the same roots are a mistake.
        let error = prepare_ruby_project(&root, Some(&roots)).unwrap_err();
        assert!(error.contains("left no Ruby source"), "{error}");
        assert!(prepare_ruby_server_project(&root, None).unwrap().is_some());
        // Roots that name the Ruby too make it a server.
        let roots = explicit_roots(&root, &["src", "app.rb"]);
        let server = prepare_ruby_server_project(&root, Some(&roots))
            .unwrap()
            .unwrap();
        assert_eq!(server.files.sources, ["app.rb"]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn explicit_roots_narrow_ruby_to_the_code_they_name() {
        // `third_party`, not `vendor`: vendor is already left out by default,
        // and a test that passed for that reason would prove nothing.
        let root = fixture("explicit-roots");
        write(&root, "Gemfile", "source 'https://rubygems.org'\n");
        write(&root, "lib/app.rb", "def f(a)\n  a && 1\nend\n");
        write(&root, "third_party/dep.rb", "def g\n  1\nend\n");

        let project = prepare_ruby_project(&root, Some(&explicit_roots(&root, &["lib"]))).unwrap();
        assert_eq!(project.files.sources, ["lib/app.rb"], "{:?}", project.files);
        assert!(
            project
                .files
                .excluded
                .contains(&("third_party/dep.rb".into(), "outside explicit source roots")),
            "{:?}",
            project.files.excluded
        );
        let scope: crate::source_discovery::SourceScope =
            serde_json::from_value(project.manifest.scope.clone().unwrap()).unwrap();
        assert_eq!(
            scope.mode,
            crate::source_discovery::SourceScopeMode::Explicit
        );
        assert_eq!(scope.roots, ["lib"]);

        // A single file is a root as well.
        let project =
            prepare_ruby_project(&root, Some(&explicit_roots(&root, &["third_party/dep.rb"])))
                .unwrap();
        assert_eq!(project.files.sources, ["third_party/dep.rb"]);
        fs::remove_dir_all(root).ok();
    }

    /// A project whose tests start a Ractor probes through the receiver a
    /// Ractor can call, statements included; one that never mentions them
    /// keeps the global and its line table.
    #[test]
    fn a_project_that_uses_ractors_probes_through_the_ractor_safe_receiver() {
        let root = fixture("ractors");
        write(&root, "lib/app.rb", "def f(a)\n  a && 1\nend\n");
        write(&root, "test/app_test.rb", "def t = 1\n");
        let plain = prepare_ruby_project(&root, None).unwrap().plan;
        assert!(!plain.ractors);
        assert_eq!(plain.receiver, RUBY_PROBE_RECEIVER);
        assert!(
            plain.files["lib/app.rb"]
                .edits
                .iter()
                .any(|edit| edit.text.contains("$__supercov."))
        );

        write(
            &root,
            "test/app_test.rb",
            "def t = Ractor.new { f(1) }.take\n",
        );
        let plan = prepare_ruby_project(&root, None).unwrap().plan;
        assert!(plan.ractors);
        assert_eq!(
            plan.receiver,
            crate::ruby_instrumenter::RUBY_RACTOR_SAFE_RECEIVER
        );
        let edits = &plan.files["lib/app.rb"].edits;
        assert!(
            edits.iter().all(|edit| !edit.text.contains("$__supercov")),
            "{edits:?}"
        );
        assert!(
            edits
                .iter()
                .any(|edit| edit.text.contains("::Supercov::RactorSafe."))
        );
        let json = serde_json::to_value(&plan).unwrap();
        assert_eq!(json["ractors"], true);
        assert!(
            serde_json::to_value(&plain)
                .unwrap()
                .get("ractors")
                .is_none()
        );
    }

    #[test]
    fn separates_sources_tests_and_tooling() {
        let root = fixture("discover");
        write(&root, "Gemfile", "source 'https://rubygems.org'\n");
        write(&root, ".rspec", "--require spec_helper\n");
        write(&root, "lib/app.rb", "def f(a)\n  a && 1\nend\n");
        write(&root, "lib/app/version.rb", "VERSION = '1'\n");
        write(&root, "spec/app_spec.rb", "RSpec.describe 'x' do end\n");
        write(&root, "spec/spec_helper.rb", "");
        write(&root, "test/app_test.rb", "");
        write(&root, "vendor/bundle/gem.rb", "x = 1\n");
        write(&root, "db/schema.rb", "x = 1\n");
        write(&root, "broken/old.rb", "def (\n");
        let project = prepare_ruby_project(&root, None).unwrap();
        assert_eq!(
            project.files.sources,
            ["broken/old.rb", "lib/app.rb", "lib/app/version.rb"]
        );
        assert_eq!(
            project.files.tests,
            [
                "spec/app_spec.rb",
                "spec/spec_helper.rb",
                "test/app_test.rb"
            ]
        );
        assert_eq!(project.files.dependency_files, [PathBuf::from("Gemfile")]);
        assert_eq!(project.files.configuration_files, [PathBuf::from(".rspec")]);
        assert_eq!(project.unparseable.len(), 1);
        assert!(project.plan.files.contains_key("lib/app.rb"));
        assert!(!project.plan.files.contains_key("broken/old.rb"));
        assert_eq!(project.manifest.limitations.len(), 1);
        assert_eq!(
            project.manifest.limitations[0]["id"],
            UNPARSEABLE_LIMITATION
        );
        // Probe keys are unique across files.
        let keys = project.plan.probes.keys().copied().collect::<Vec<_>>();
        let unique = keys.iter().collect::<BTreeSet<_>>();
        assert_eq!(keys.len(), unique.len());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn tests_helpers_and_gemspecs_are_known_by_their_names() {
        let root = fixture("names");
        write(&root, "lib/a.rb", "A = 1\n");
        write(&root, "lib/a_test.rb", "A = 1\n");
        write(&root, "test_b.rb", "B = 1\n");
        write(&root, "spec_helper.rb", "require 'rspec'\n");
        write(&root, "thing.gemspec", "Gem::Specification.new\n");
        let files = discover_ruby_files(&root).unwrap();
        assert_eq!(files.sources, ["lib/a.rb"]);
        assert_eq!(
            files.tests,
            ["lib/a_test.rb", "spec_helper.rb", "test_b.rb"]
        );
        assert_eq!(files.dependency_files, [PathBuf::from("thing.gemspec")]);
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_limitation_two_constructs_share_is_declared_once() {
        let root = fixture("limitations");
        assert!(
            prepare_ruby_project(&root, None)
                .unwrap_err()
                .starts_with("no Ruby source files were found")
        );
        // Tests alone are a project.
        write(&root, "test/a_test.rb", "A = 1\n");
        assert!(prepare_ruby_project(&root, None).is_ok());
        write(
            &root,
            "lib/workers.rb",
            "def run(x)\n  a = Ractor.new(x) { |y| y ? 1 : 2 }\n  b = Ractor.new(x) { |y| y ? 3 : 4 }\n  [a.take, b.take]\nend\n",
        );
        let project = prepare_ruby_project(&root, None).unwrap();
        let ids = project
            .manifest
            .limitations
            .iter()
            .map(|limitation| limitation["id"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, [crate::ruby_instrumenter::RACTOR_BLOCK_LIMITATION]);
        fs::remove_dir_all(&root).unwrap();
    }
}
