//! Cargo workspace discovery and isolated owned-Rust frontend preparation.

use crate::fault_fs as fs;
use crate::fault_process::Launch;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Component, Path, PathBuf},
    process::Command,
};

use ra_ap_syntax::{
    AstNode, AstToken, Edition, SourceFile, SyntaxKind,
    ast::{self, HasAttrs, HasModuleItem, HasName},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    coverage_report::CoverageManifest,
    rust_instrumenter::{instrument_rust_file_numbered, test_code_path, test_item},
    rust_runtime::render_rust_region_runtime,
};

const RUST_APPLICATION_SOURCE: &str = "Rust application source";
/// A test, benchmark or example target, or a test module in a file of its
/// own: read for where its assertions stand, and not measured.
const RUST_TEST_CODE: &str = "test, benchmark or example code";
/// Why a file the module tree reaches is not measured: a bench or an example
/// `cargo test` does not build, or a module behind a `#[cfg]` that is off.
pub const RUST_NOT_COMPILED: &str = "not compiled by this build";

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRustProject {
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    pub source_files: Vec<String>,
    pub crate_roots: Vec<String>,
    pub runtime_module: String,
    pub manifest: CoverageManifest,
    /// The probes' numbering: a region's slots, by ordinal.
    pub probes: crate::rust_instrumenter::RustProbeLayout,
    pub preparation: RustPreparationTimings,
}

#[cfg(test)]
impl PreparedRustProject {
    /// A workspace with nothing instrumented, for tests of what runs in it.
    pub(crate) fn empty(workspace_root: PathBuf) -> Self {
        Self {
            target_directory: workspace_root.join("target"),
            workspace_root,
            source_files: Vec::new(),
            crate_roots: Vec::new(),
            runtime_module: String::new(),
            manifest: CoverageManifest {
                unmeasured: Vec::new(),
                decisions: Vec::new(),
                points: Vec::new(),
                branches: Vec::new(),
                limitations: Vec::new(),
                scope: None,
                assertion_sites: Vec::new(),
            },
            probes: Default::default(),
            preparation: Default::default(),
        }
    }
}

/// Where preparing the instrumented workspace spent its time, for
/// `SUPERCOV_PHASE_TIMING=1`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RustPreparationTimings {
    pub metadata_ms: f64,
    pub discovery_ms: f64,
    pub instrument_ms: f64,
    pub runtime_ms: f64,
}

#[derive(Debug)]
pub enum RustProjectError {
    Io { path: PathBuf, reason: String },
    MetadataLaunch(String),
    MetadataFailed(String),
    MetadataJson(String),
    UnsafePath(String),
    NoWorkspacePackages,
    NoSourceFiles,
    Instrument { file: String, reason: String },
    DuplicateObligation(String),
    SourceRoots(String),
}

impl std::fmt::Display for RustProjectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, reason } => write!(formatter, "{}: {reason}", path.display()),
            Self::MetadataLaunch(reason) => {
                write!(formatter, "could not launch cargo metadata: {reason}")
            }
            Self::MetadataFailed(reason) => write!(formatter, "cargo metadata failed: {reason}"),
            Self::MetadataJson(reason) => write!(formatter, "invalid cargo metadata: {reason}"),
            Self::UnsafePath(path) => {
                write!(formatter, "Cargo reported an unsafe workspace path: {path}")
            }
            Self::NoWorkspacePackages => {
                write!(formatter, "Cargo metadata reported no workspace packages")
            }
            Self::NoSourceFiles => write!(
                formatter,
                "Cargo workspace contains no owned Rust source files"
            ),
            Self::Instrument { file, reason } => {
                write!(formatter, "could not instrument {file}: {reason}")
            }
            Self::DuplicateObligation(id) => {
                write!(formatter, "duplicate Rust obligation ID: {id}")
            }
            Self::SourceRoots(reason) => write!(formatter, "{reason}"),
        }
    }
}

impl std::error::Error for RustProjectError {}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    workspace_root: PathBuf,
}

/// Where Cargo builds the copy: inside it, named to Cargo by environment,
/// which no configuration of the project overrides.
fn copy_target_directory(root: &Path) -> PathBuf {
    root.join(".supercov/rust-target")
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
    #[serde(default)]
    features: BTreeMap<String, Vec<String>>,
}

#[derive(Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
    src_path: PathBuf,
}

#[track_caller]
fn canonical_directory(path: &Path) -> Result<PathBuf, RustProjectError> {
    fs::canonicalize(path).map_err(|error| RustProjectError::Io {
        path: path.to_owned(),
        reason: error.to_string(),
    })
}

fn confined_relative(root: &Path, path: &Path) -> Result<String, RustProjectError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| RustProjectError::UnsafePath(path.display().to_string()))?;
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(RustProjectError::UnsafePath(path.display().to_string()));
    }
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

fn cargo_metadata(root: &Path) -> Result<CargoMetadata, RustProjectError> {
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", copy_target_directory(root))
        .launch_output()
        .map_err(|error| RustProjectError::MetadataLaunch(error.to_string()))?;
    if !output.status.success() {
        return Err(RustProjectError::MetadataFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| RustProjectError::MetadataJson(error.to_string()))
}

/// The workspace sources the build actually read, from the depinfo rustc
/// writes beside every artifact (`<artifact>.d`: a make rule whose
/// prerequisites are every file that went into it). `None` when no depinfo
/// could be read, which means "do not prune".
///
/// Source discovery follows `mod` declarations because that is what rustc
/// RESOLVES; this is what it COMPILES. A module behind a `#[cfg]` that is off
/// is resolved and never built, and its obligations can never be covered --
/// smallvec's `serde`, `borsh`, `rayon` and `specialization` modules are 142
/// such lines. Reading the build is exact for every `cfg` predicate, where
/// evaluating them here would have to reproduce Cargo's feature resolution.
pub fn compiled_source_files(
    workspace: &Path,
    target_directory: &Path,
) -> Option<BTreeSet<String>> {
    let mut compiled = BTreeSet::new();
    let depinfo_files = collect_depinfo_files(target_directory, workspace, &mut compiled);
    (depinfo_files > 0).then_some(compiled)
}

/// Read every depinfo file under `directory` into `compiled`; how many
/// there were.
fn collect_depinfo_files(
    directory: &Path,
    workspace: &Path,
    compiled: &mut BTreeSet<String>,
) -> usize {
    let Ok(entries) = fs::read_dir(directory) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            match entry.file_type() {
                Ok(file_type) if file_type.is_dir() => {
                    collect_depinfo_files(&path, workspace, compiled)
                }
                Ok(file_type)
                    if file_type.is_file()
                        && path.extension().is_some_and(|extension| extension == "d") =>
                {
                    fs::read_to_string(&path).map_or(0, |text| {
                        collect_depinfo_sources(&text, workspace, compiled);
                        1
                    })
                }
                _ => 0,
            }
        })
        .sum()
}

/// The prerequisites of every rule in a depinfo file, as workspace-relative
/// paths. A rule is `target: prerequisite prerequisite`, and a space inside a
/// path is escaped as `\ `.
fn collect_depinfo_sources(text: &str, workspace: &Path, compiled: &mut BTreeSet<String>) {
    for line in text.lines() {
        // A prerequisite-only line (`path:` with nothing after it) carries no
        // sources, and a target's own path is not a source.
        let Some((_, prerequisites)) = line.split_once(": ") else {
            continue;
        };
        let mut current = String::new();
        let mut characters = prerequisites.chars().peekable();
        while let Some(character) = characters.next() {
            match character {
                '\\' if characters.peek() == Some(&' ') => {
                    characters.next();
                    current.push(' ');
                }
                ' ' => {
                    push_workspace_source(&current, workspace, compiled);
                    current.clear();
                }
                _ => current.push(character),
            }
        }
        push_workspace_source(&current, workspace, compiled);
    }
}

fn push_workspace_source(path: &str, workspace: &Path, compiled: &mut BTreeSet<String>) {
    let path = path.trim();
    if path.is_empty() || !path.ends_with(".rs") {
        return;
    }
    // Cargo runs rustc with the workspace as its working directory, so a
    // source of the crate being built is named relatively and a dependency's
    // source -- from the registry -- absolutely.
    let candidate = Path::new(path);
    let relative = if candidate.is_absolute() {
        let Ok(relative) = candidate.strip_prefix(workspace) else {
            return;
        };
        relative
    } else {
        candidate
    };
    // A part of a `str` cut at a separator is still UTF-8: nothing is lost.
    compiled.insert(relative.to_string_lossy().replace('\\', "/"));
}

/// The files rustc compiles for the given crate roots: each root and,
/// transitively, every module it declares with `mod name;` (resolved the way
/// rustc resolves it, `#[path]` included) and every file it pulls in with a
/// literal `include!("....rs")`. A `.rs` file under the package that no module
/// reaches -- a runtime source embedded as data with `include_str!`, a test
/// fixture, a snippet -- is not part of any crate, so instrumenting it would
/// change the data and count code that is never compiled.
///
/// A file that does not exist is skipped, not an error: a `#[cfg]`-gated
/// module may name a file the checkout lacks, and rustc only complains when
/// that cfg is active. Files outside the workspace are left alone as well.
/// Every file the crate roots reach. `tests` gets those only test code
/// reaches -- through a test target's root or a `#[cfg(test)]` module --
/// which are test code whatever their names. Every other route is followed
/// first, so a file reached both ways is not test code. `owners` gets the
/// crate roots that compile each file: one file can be in several crates.
fn resolve_module_tree(
    workspace: &Path,
    roots: &BTreeSet<PathBuf>,
    test_roots: &BTreeSet<PathBuf>,
    files: &mut BTreeSet<PathBuf>,
    tests: &mut BTreeSet<PathBuf>,
    owners: &mut BTreeMap<PathBuf, BTreeSet<PathBuf>>,
) -> Result<(), RustProjectError> {
    let canonical_workspace = canonical_directory(workspace)?;
    // (file, directory its `mod` children resolve in, crate root)
    let (mut pending, mut pending_tests): (Vec<_>, Vec<_>) = roots
        .iter()
        .map(|root| (root.clone(), owner_directory(root), root.clone()))
        .partition(|(root, _, _)| !test_roots.contains(root));
    let mut visited = BTreeSet::new();
    loop {
        let (file, directory, root, test) = match pending.pop() {
            Some((file, directory, root)) => (file, directory, root, false),
            None => match pending_tests.pop() {
                Some((file, directory, root)) => (file, directory, root, true),
                None => break,
            },
        };
        // `#[path = "../src/shared.rs"]` climbs out of its directory; the
        // path is normalised lexically so the workspace check and the file
        // set see one spelling of it.
        let file = normalize(&file);
        let directory = normalize(&directory);
        if !file.starts_with(workspace) {
            continue;
        }
        let Ok(metadata) = fs::symlink_metadata(&file) else {
            continue;
        };
        // A symlink is followed only within the workspace: crossbeam shares
        // one source file between its crates that way. The file is recorded
        // under its target's path, so it is instrumented and digested once as
        // a regular file; a symlink leaving the workspace would be
        // instrumented in place, outside the copy, and is refused.
        let file = if metadata.file_type().is_symlink() {
            let target = fs::canonicalize(&file).map_err(|error| RustProjectError::Io {
                path: file.clone(),
                reason: error.to_string(),
            })?;
            if !target.starts_with(&canonical_workspace) || !target.is_file() {
                return Err(RustProjectError::UnsafePath(file.display().to_string()));
            }
            target
        } else if metadata.is_file() {
            file.clone()
        } else {
            continue;
        };
        // A file is test code by the first route that reaches it; it is
        // walked again for every crate that reaches it, to find its owners.
        if files.insert(file.clone()) && test {
            tests.insert(file.clone());
        }
        if !visited.insert((file.clone(), root.clone())) {
            continue;
        }
        owners.entry(file.clone()).or_default().insert(root.clone());
        let source = fs::read_to_string(&file).map_err(|error| RustProjectError::Io {
            path: file.clone(),
            reason: error.to_string(),
        })?;
        let parsed = SourceFile::parse(&source, Edition::CURRENT).tree();
        let mut declared = Vec::new();
        collect_module_declarations(
            parsed.items(),
            &file,
            &directory,
            false,
            false,
            &mut declared,
        );
        for (child, owner, gated) in declared {
            if test || gated {
                pending_tests.push((child, owner, root.clone()));
            } else {
                pending.push((child, owner, root.clone()));
            }
        }
    }
    Ok(())
}

/// Resolve `.` and `..` components without touching the filesystem.
fn normalize(path: &Path) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                normalized.pop();
            }
            Component::CurDir => {}
            other => normalized.push(other.as_os_str()),
        }
    }
    normalized
}

fn owner_directory(file: &Path) -> PathBuf {
    file.parent().map_or_else(PathBuf::new, Path::to_path_buf)
}

/// Walk the items of one module body. `directory` is where this module's
/// `mod name;` children live; `inline` says whether we are inside a
/// `mod name { ... }` block, which changes what `#[path]` is relative to.
/// `gated` says whether the body is test code, and is passed on with each
/// declared file.
fn collect_module_declarations(
    items: impl Iterator<Item = ast::Item>,
    file: &Path,
    directory: &Path,
    inline: bool,
    gated: bool,
    pending: &mut Vec<(PathBuf, PathBuf, bool)>,
) {
    for item in items {
        match item {
            ast::Item::Module(module) => {
                let Some(name) = module.name() else {
                    continue;
                };
                let gated = gated || test_item(module.syntax());
                let name = name.text().to_string();
                let path_attribute = module.attrs().find_map(|attr| {
                    let is_path = attr
                        .path()
                        .is_some_and(|path| path.syntax().text() == "path");
                    is_path.then(|| string_literal(attr.syntax())).flatten()
                });
                if let Some(list) = module.item_list() {
                    let nested = directory.join(&name);
                    collect_module_declarations(list.items(), file, &nested, true, gated, pending);
                } else if let Some(path) = path_attribute {
                    // Relative to the file's own directory at the top level,
                    // to the inline module's directory inside a block; the
                    // loaded file owns its directory like a `mod.rs` does.
                    let base = if inline {
                        directory.to_path_buf()
                    } else {
                        owner_directory(file)
                    };
                    let target = base.join(path);
                    let owner = owner_directory(&target);
                    pending.push((target, owner, gated));
                } else {
                    // `name.rs` and `name/mod.rs` both put their children in
                    // `directory/name/`.
                    let children = directory.join(&name);
                    pending.push((
                        directory.join(format!("{name}.rs")),
                        children.clone(),
                        gated,
                    ));
                    pending.push((children.join("mod.rs"), children, gated));
                }
            }
            ast::Item::MacroCall(call) => {
                let is_include = call.path().is_some_and(|path| {
                    matches!(
                        path.syntax().text().to_string().as_str(),
                        "include" | "std::include" | "core::include" | "::std::include"
                    )
                });
                if !is_include {
                    // A macro that selects among implementations declares its
                    // modules in its token tree, where no item walk reaches
                    // them: hashbrown's `cfg_select!` and the `cfg_if!` in
                    // crates everywhere. Both arms are followed; the build
                    // decides which one counts.
                    for name in token_tree_modules(call.syntax()) {
                        let children = directory.join(&name);
                        pending.push((
                            directory.join(format!("{name}.rs")),
                            children.clone(),
                            gated,
                        ));
                        pending.push((children.join("mod.rs"), children, gated));
                    }
                    continue;
                }
                let Some(literal) = string_literal(call.syntax()) else {
                    continue;
                };
                if !literal.ends_with(".rs") {
                    continue;
                }
                // Included code is spliced into this module: its own `mod`
                // declarations resolve where this module's do.
                pending.push((
                    owner_directory(file).join(literal),
                    directory.to_path_buf(),
                    gated,
                ));
            }
            _ => {}
        }
    }
}

/// Every `mod <name>;` declared inside a macro's token tree. The tokens are
/// unparsed there, so the sequence is matched directly.
fn token_tree_modules(node: &ra_ap_syntax::SyntaxNode) -> Vec<String> {
    let mut names = Vec::new();
    let mut tokens = node
        .descendants_with_tokens()
        .filter_map(|element| element.into_token())
        .filter(|token| !token.kind().is_trivia())
        .peekable();
    loop {
        let Some(token) = tokens.next() else {
            break;
        };
        if token.text() != "mod" {
            continue;
        }
        let Some(name) = tokens
            .peek()
            .filter(|next| next.kind() == SyntaxKind::IDENT)
        else {
            continue;
        };
        let name = name.text().to_string();
        tokens.next();
        // `mod name;` declares a file; `mod name { .. }` is inline and its
        // items are already in this tree.
        if tokens
            .peek()
            .is_some_and(|next| next.kind() == SyntaxKind::SEMICOLON)
        {
            tokens.next();
            names.push(name);
        }
    }
    names
}

/// The first string literal under a node, unescaped. Inside a macro's token
/// tree the literal is a bare token, not a `Literal` node, so look at tokens.
fn string_literal(node: &ra_ap_syntax::SyntaxNode) -> Option<String> {
    node.descendants_with_tokens().find_map(|element| {
        let string = ast::String::cast(element.into_token()?)?;
        string.value().ok().map(|value| value.into_owned())
    })
}

/// The crate roots of every workspace member: the source file of each Cargo
/// target except build scripts, which Cargo compiles and runs on their own.
/// The crate roots of proc-macro targets. What they compile to is a compiler
/// plugin: rustc loads it while building the crate under test and runs it
/// there, so no test process ever executes it and the owned probes see
/// nothing. `crate_roots` has already confined every one of them to the
/// workspace.
/// Every crate's root, confined to the workspace, and the proc-macro crates'
/// among them.
type CrateRoots = (BTreeSet<PathBuf>, BTreeSet<PathBuf>);

/// What every path under the workspace that a crate's module tree reached,
/// or a crate root already confined, is.
const INSIDE: &str = "a path the workspace's crates reach is inside it";

fn crate_roots(
    workspace: &Path,
    packages: &[CargoPackage],
) -> Result<CrateRoots, RustProjectError> {
    let mut roots = BTreeSet::new();
    let mut proc_macros = BTreeSet::new();
    packages.iter().try_for_each(|package| {
        let directory = package
            .manifest_path
            .parent()
            .expect("Cargo names a manifest by its absolute path");
        let directory = canonical_directory(directory)?;
        confined_relative(workspace, &directory).or_else(|error| {
            (directory == workspace)
                .then_some(String::new())
                .ok_or(error)
        })?;
        package
            .targets
            .iter()
            .filter(|target| !target.kind.iter().any(|kind| kind == "custom-build"))
            .try_for_each(|target| {
                let root =
                    fs::canonicalize(&target.src_path).map_err(|error| RustProjectError::Io {
                        path: target.src_path.clone(),
                        reason: error.to_string(),
                    })?;
                confined_relative(workspace, &root)?;
                if target.kind.iter().any(|kind| kind == "proc-macro") {
                    proc_macros.insert(root.clone());
                }
                roots.insert(root);
                Ok(())
            })
    })?;
    Ok((roots, proc_macros))
}

/// The library roots of the workspace's Rust Workers, each with the cfg of
/// the build that compiles it: worker-build's, for wasm, whatever target
/// `cargo test` builds the rest for.
fn worker_library_roots(
    packages: &[CargoPackage],
) -> BTreeMap<PathBuf, crate::rust_instrumenter::TargetCfg> {
    packages
        .iter()
        .filter_map(|package| {
            let directory = package
                .manifest_path
                .parent()
                .expect("Cargo names a manifest by its absolute path");
            let cfg = crate::rust_worker::worker_build_cfg(directory, &package.features)?;
            Some(
                package
                    .targets
                    .iter()
                    .filter(|target| {
                        target.kind.iter().any(|kind| {
                            matches!(
                                kind.as_str(),
                                "lib" | "rlib" | "cdylib" | "dylib" | "staticlib"
                            )
                        })
                    })
                    .filter_map(|target| fs::canonicalize(&target.src_path).ok())
                    .map(move |root| (root, cfg.clone())),
            )
        })
        .flatten()
        .collect()
}

/// The crate roots of test, benchmark and example targets, whose module
/// trees are test code.
fn test_target_roots(packages: &[CargoPackage]) -> BTreeSet<PathBuf> {
    packages
        .iter()
        .flat_map(|package| &package.targets)
        .filter(|target| {
            target
                .kind
                .iter()
                .any(|kind| matches!(kind.as_str(), "test" | "bench" | "example"))
        })
        .filter_map(|target| fs::canonicalize(&target.src_path).ok())
        .collect()
}

fn runtime_module_name(sources: &BTreeMap<String, String>) -> String {
    let mut suffix = 0_usize;
    loop {
        let candidate = if suffix == 0 {
            "__supercov_runtime_v1".to_owned()
        } else {
            format!("__supercov_runtime_v1_{suffix}")
        };
        if sources.values().all(|source| !source.contains(&candidate)) {
            return candidate;
        }
        suffix += 1;
    }
}

/// Twelve hex digits identifying an instrumentation: a digest of every
/// obligation ID in the manifest. Two builds of the same sources share it;
/// any other program's instrumentation, such as a fixture a test prepares
/// and runs, has another.
/// Obligations in a proc-macro crate leave the denominator: the compiler runs
/// that code while it builds the crate under test, so a test process cannot
/// observe it. They stay in the manifest -- the evidence files are named by a
/// token over its obligation IDs -- and are declined, with one limitation per
/// file saying why.
fn decline_proc_macro_obligations(
    workspace: &Path,
    proc_macro_roots: &BTreeSet<PathBuf>,
    manifest: &mut CoverageManifest,
) {
    if proc_macro_roots.is_empty() {
        return;
    }
    let mut reached = BTreeSet::new();
    resolve_module_tree(
        workspace,
        proc_macro_roots,
        &BTreeSet::new(),
        &mut reached,
        &mut BTreeSet::new(),
        &mut BTreeMap::new(),
    )
    .expect("the proc-macro crates' modules were resolved with every crate's");
    let files = reached
        .iter()
        .map(|path| confined_relative(workspace, path).expect(INSIDE))
        .collect::<BTreeSet<_>>();
    let mut unmeasured = manifest.unmeasured.iter().cloned().collect::<BTreeSet<_>>();
    unmeasured.extend(
        manifest
            .points
            .iter()
            .filter(|point| files.contains(&point.file))
            .map(|point| point.id.clone()),
    );
    unmeasured.extend(
        manifest
            .decisions
            .iter()
            .filter(|decision| files.contains(&decision.file))
            .map(|decision| decision.id.clone()),
    );
    unmeasured.extend(
        manifest
            .branches
            .iter()
            .filter(|branch| files.contains(&branch.file))
            .map(|branch| branch.id.clone()),
    );
    manifest.unmeasured = unmeasured.into_iter().collect();
    // The boundaries already declared inside these files say nothing now.
    manifest.limitations.retain(|limitation| {
        limitation
            .get("file")
            .and_then(|file| file.as_str())
            .is_none_or(|file| !files.contains(file))
    });
    manifest.limitations.extend(files.into_iter().map(|file| {
        serde_json::json!({
            "id": format!("rust-proc-macro-runs-in-the-compiler#{file}"),
            "kind": "source-scope",
            "file": file,
            "line": 1,
            "column": 0,
            "source": "",
            "blocking": false,
            "reason": "This crate compiles to a compiler plugin: rustc loads it and runs it while building the crate under test, so no test process executes it"
        })
    }));
}

pub fn manifest_token(manifest: &CoverageManifest) -> String {
    let mut ids = manifest
        .points
        .iter()
        .map(|point| point.id.as_str())
        .chain(
            manifest
                .decisions
                .iter()
                .map(|decision| decision.id.as_str()),
        )
        .chain(manifest.branches.iter().flat_map(|branch| {
            branch
                .alternatives
                .iter()
                .map(|alternative| alternative.id.as_str())
        }))
        .collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    let mut hasher = Sha256::new();
    for id in ids {
        hasher.update(id.as_bytes());
        hasher.update(b"\n");
    }
    hex(&hasher.finalize()[..6])
}

/// The runtime names its evidence files `<crate key>-<pid>.events`; the key
/// is the manifest token followed by a digest of the crate root, so the
/// reader can tell this instrumentation's files from any other's and two
/// crates of one process write separate files.
fn crate_key(token: &str, path: &str) -> String {
    format!("{token}{}", hex(&Sha256::digest(path.as_bytes())[..6]))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn merge_manifest(
    destination: &mut CoverageManifest,
    mut source: CoverageManifest,
) -> Result<(), RustProjectError> {
    let mut ids = destination
        .points
        .iter()
        .map(|point| point.id.as_str())
        .chain(
            destination
                .decisions
                .iter()
                .map(|decision| decision.id.as_str()),
        )
        .chain(destination.branches.iter().map(|branch| branch.id.as_str()))
        .collect::<BTreeSet<_>>();
    for id in source
        .points
        .iter()
        .map(|point| point.id.as_str())
        .chain(source.decisions.iter().map(|decision| decision.id.as_str()))
        .chain(source.branches.iter().map(|branch| branch.id.as_str()))
    {
        if !ids.insert(id) {
            return Err(RustProjectError::DuplicateObligation(id.into()));
        }
    }
    destination.points.append(&mut source.points);
    destination
        .assertion_sites
        .append(&mut source.assertion_sites);
    destination.decisions.append(&mut source.decisions);
    destination.branches.append(&mut source.branches);
    // Obligations the file declined -- a `const fn` body, a `GlobalAlloc`
    // implementation -- are declined for the project too.
    destination.unmeasured.append(&mut source.unmeasured);
    // A limitation is one site in one file. Deduping on the id alone kept a
    // single macro-expansion limitation for the whole project, charged to
    // whichever file merged first.
    let site = |value: &serde_json::Value| {
        (
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_owned),
            value
                .get("file")
                .and_then(|file| file.as_str())
                .map(str::to_owned),
            value.get("line").and_then(serde_json::Value::as_u64),
            value.get("column").and_then(serde_json::Value::as_u64),
        )
    };
    for limitation in source.limitations {
        let key = site(&limitation);
        if !destination
            .limitations
            .iter()
            .any(|existing| site(existing) == key)
        {
            destination.limitations.push(limitation);
        }
    }
    Ok(())
}

pub fn prepare_rust_project(
    workspace: &Path,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
) -> Result<PreparedRustProject, RustProjectError> {
    prepare_rust_project_for(workspace, roots, None)
}

/// `prepare_rust_project` for a build whose target cfg values are known:
/// items its `#[cfg(..)]` leaves uncompiled are not measured.
pub fn prepare_rust_project_for(
    workspace: &Path,
    roots: Option<&crate::source_discovery::ExplicitSourceRoots>,
    cfg: Option<&crate::rust_instrumenter::TargetCfg>,
) -> Result<PreparedRustProject, RustProjectError> {
    // Named apart from the crate roots below, which reuse `roots`.
    let source_roots = roots;
    let elapsed = |started: std::time::Instant| started.elapsed().as_secs_f64() * 1000.0;
    let mut preparation = RustPreparationTimings::default();
    let workspace = canonical_directory(workspace)?;
    let started = std::time::Instant::now();
    let metadata = cargo_metadata(&workspace)?;
    preparation.metadata_ms = elapsed(started);
    let metadata_root = canonical_directory(&metadata.workspace_root)?;
    if metadata_root != workspace {
        return Err(RustProjectError::UnsafePath(
            metadata.workspace_root.display().to_string(),
        ));
    }
    let members = metadata
        .workspace_members
        .into_iter()
        .collect::<BTreeSet<_>>();
    let packages = metadata
        .packages
        .into_iter()
        .filter(|package| members.contains(&package.id))
        .collect::<Vec<_>>();
    if packages.is_empty() {
        return Err(RustProjectError::NoWorkspacePackages);
    }

    let started = std::time::Instant::now();
    let (roots, proc_macro_roots) = crate_roots(&workspace, &packages)?;
    let worker_roots = worker_library_roots(&packages);
    let mut files = BTreeSet::new();
    let mut tests = BTreeSet::new();
    let mut owners = BTreeMap::new();
    resolve_module_tree(
        &workspace,
        &roots,
        &test_target_roots(&packages),
        &mut files,
        &mut tests,
        &mut owners,
    )?;
    // Which crates compile each file, by the name the gates carry.
    let owners = owners
        .into_iter()
        .filter_map(|(path, roots)| {
            confined_relative(&workspace, &path)
                .ok()
                .map(|relative| (relative, roots))
        })
        .collect::<BTreeMap<_, _>>();
    // Test code is instrumented for its assertion markers alone.
    let tests = tests
        .iter()
        .map(|path| confined_relative(&workspace, path).expect(INSIDE))
        .collect::<BTreeSet<_>>();
    if files.is_empty() {
        return Err(RustProjectError::NoSourceFiles);
    }

    let mut sources = files
        .into_iter()
        .map(|path| {
            let relative = confined_relative(&workspace, &path).expect(INSIDE);
            let source = fs::read_to_string(&path).map_err(|error| RustProjectError::Io {
                path: path.clone(),
                reason: error.to_string(),
            })?;
            Ok((relative, source))
        })
        .collect::<Result<BTreeMap<_, _>, RustProjectError>>()?;
    preparation.discovery_ms = elapsed(started);
    let started = std::time::Instant::now();
    let runtime_module = runtime_module_name(&sources);
    // Narrowed only after the runtime's name is chosen: that name must not
    // collide with any module in the crate, including those the roots leave
    // out. A module left out is simply not instrumented, and compiles as the
    // author wrote it.
    let mut outside = Vec::new();
    if let Some(source_roots) = source_roots {
        let mut kept = sources.keys().cloned().collect::<Vec<_>>();
        source_roots.narrow(&mut kept, &mut outside, String::as_str);
        let kept = kept.into_iter().collect::<BTreeSet<_>>();
        sources.retain(|relative, _| kept.contains(relative));
        source_roots
            .refuse_if_empty(sources.len(), "Rust")
            .map_err(RustProjectError::SourceRoots)?;
    }
    let runtime_path = format!("crate::{runtime_module}");
    let mut manifest = CoverageManifest {
        unmeasured: Vec::new(),
        decisions: Vec::new(),
        points: Vec::new(),
        branches: Vec::new(),
        limitations: Vec::new(),
        scope: None,
        assertion_sites: Vec::new(),
    };
    // One numbering spans the workspace: a file may be compiled into several
    // crates, and every crate's runtime maps the same region.
    let mut probes = crate::rust_instrumenter::RustProbeLayout::default();
    // Never none: no source left by the roots was refused above.
    sources.iter().try_for_each(|(relative, source)| -> Result<(), RustProjectError> {
        let test_file = tests.contains(relative) || test_code_path(relative);
        // A file is compiled as each crate that holds it is built: a Worker's
        // library by worker-build, the rest for the command's target. Items
        // are left out only where every one of those builds leaves them out.
        // A file the module tree reached has the crate it reached it from.
        let builds = owners[relative]
            .iter()
            .map(|root| worker_roots.get(root).or(cfg))
            .collect::<Vec<_>>();
        let (first, rest) = builds.split_first().expect("a file has a crate");
        let file_cfg = rest
            .iter()
            .all(|other| other == first)
            .then_some(*first)
            .flatten();
        let transformed = match instrument_rust_file_numbered(
            relative,
            source,
            &runtime_path,
            test_file,
            file_cfg,
            &mut probes,
        ) {
            Ok(transformed) => transformed,
            // A bug in Supercov, kept to this file: it compiles as written,
            // and the run says nothing in it is measured.
            Err(error @ crate::rust_instrumenter::RustInstrumenterError::Internal(_)) => {
                manifest.limitations.push(serde_json::json!({
                    "id": format!("rust-file-not-instrumented#{relative}"),
                    "kind": "source-scope",
                    "file": relative,
                    "line": 1,
                    "column": 0,
                    "source": "",
                    "blocking": false,
                    "reason": format!("Supercov hit {error} instrumenting this file, a bug to report; it compiles as written and nothing in it is measured"),
                }));
                return Ok(());
            }
            Err(error) => {
                return Err(RustProjectError::Instrument {
                    file: relative.clone(),
                    reason: error.to_string(),
                });
            }
        };
        merge_manifest(&mut manifest, transformed.manifest)
            .expect("obligation ids carry their file, so files never share one");
        fs::write(workspace.join(relative), transformed.code).map_err(|error| {
            RustProjectError::Io {
                path: workspace.join(relative),
                reason: error.to_string(),
            }
        })?;
        Ok(())
    })?;

    preparation.instrument_ms = elapsed(started);
    let started = std::time::Instant::now();
    decline_proc_macro_obligations(&workspace, &proc_macro_roots, &mut manifest);

    let token = manifest_token(&manifest);
    probes.token = token.clone();
    let mut crate_roots = Vec::new();
    let mut crate_files = Vec::new();
    // A workspace has a crate at least.
    roots
        .into_iter()
        .try_for_each(|root| -> Result<(), RustProjectError> {
            let relative = confined_relative(&workspace, &root).expect(INSIDE);
            // The crate answers `cfg!` for the gates in the files it compiles.
            let crate_cfgs = probes
                .cfgs
                .iter()
                .enumerate()
                .filter(|(_, gate)| {
                    owners
                        .get(&gate.file)
                        .is_some_and(|roots| roots.contains(&root))
                })
                .map(|(ordinal, _)| ordinal)
                .collect::<Vec<_>>();
            let key = crate_key(&token, &relative);
            if worker_roots.contains_key(&root) {
                probes.workers.insert(key.clone());
            }
            let runtime =
                render_rust_region_runtime(&runtime_module, &token, &key, &probes, &crate_cfgs);
            crate_files.push((
                key,
                owners
                    .iter()
                    .filter(|(_, roots)| roots.contains(&root))
                    .map(|(file, _)| file.clone())
                    .collect::<Vec<_>>(),
            ));
            let mut source = fs::read_to_string(&root).map_err(|error| RustProjectError::Io {
                path: root.clone(),
                reason: error.to_string(),
            })?;
            source.push('\n');
            source.push_str(&runtime);
            fs::write(&root, source).map_err(|error| RustProjectError::Io {
                path: root,
                reason: error.to_string(),
            })?;
            crate_roots.push(relative);
            Ok(())
        })?;
    preparation.runtime_ms = elapsed(started);
    probes.crates = crate_files.into_iter().collect();

    manifest
        .points
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest
        .assertion_sites
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest
        .decisions
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest
        .branches
        .sort_by(|left, right| left.id.cmp(&right.id));
    manifest.limitations.sort_by(|left, right| {
        left.get("id")
            .and_then(|value| value.as_str())
            .cmp(&right.get("id").and_then(|value| value.as_str()))
    });
    let target_directory = copy_target_directory(&workspace);
    let kept = sources.keys().cloned().collect::<Vec<_>>();
    let scope = match source_roots {
        Some(source_roots) => source_roots
            .scope(&kept, &outside)
            .map_err(|error| RustProjectError::SourceRoots(error.to_string()))?,
        // What the build then does not compile is taken out where the build
        // is known: see `RUST_NOT_COMPILED`.
        None => {
            let (test_code, measured) = kept
                .into_iter()
                .partition::<Vec<_>, _>(|file| tests.contains(file) || test_code_path(file));
            crate::source_discovery::automatic_scope(
                measured
                    .into_iter()
                    .map(|file| (file, RUST_APPLICATION_SOURCE.to_owned())),
                test_code
                    .into_iter()
                    .map(|file| (file, RUST_TEST_CODE.to_owned())),
            )
        }
    };
    manifest.scope = Some(serde_json::to_value(scope).expect("plain data serializes"));
    Ok(PreparedRustProject {
        workspace_root: workspace,
        target_directory,
        source_files: sources.into_keys().collect(),
        crate_roots,
        runtime_module,
        manifest,
        probes,
        preparation,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        process::Command,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    #[test]
    fn each_rust_project_error_says_what_failed() {
        use super::RustProjectError as E;
        let messages = [
            E::Io {
                path: std::path::PathBuf::from("Cargo.toml"),
                reason: "gone".into(),
            },
            E::MetadataLaunch("no cargo".into()),
            E::MetadataFailed("exit 101".into()),
            E::MetadataJson("eof".into()),
            E::UnsafePath("../x".into()),
            E::NoWorkspacePackages,
            E::NoSourceFiles,
            E::Instrument {
                file: "src/lib.rs".into(),
                reason: "parse".into(),
            },
            E::DuplicateObligation("o1".into()),
            E::SourceRoots("no roots".into()),
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "Cargo.toml: gone",
                "could not launch cargo metadata: no cargo",
                "cargo metadata failed: exit 101",
                "invalid cargo metadata: eof",
                "Cargo reported an unsafe workspace path: ../x",
                "Cargo metadata reported no workspace packages",
                "Cargo workspace contains no owned Rust source files",
                "could not instrument src/lib.rs: parse",
                "duplicate Rust obligation ID: o1",
                "no roots",
            ]
        );
    }

    use super::*;

    fn fixture() -> PathBuf {
        // One test calls this today, so nothing can collide with it yet. The
        // counter is here because the clock is not enough on its own: it ticks
        // once per microsecond and every test shares the pid, so the second
        // test to use this helper would draw the same root as the first when
        // the two start together.
        static UNIQUE: AtomicU64 = AtomicU64::new(0);
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-project-{}-{nonce}-{}",
            std::process::id(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("src")).unwrap();
        fs::create_dir(root.join("tests")).unwrap();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='rust-project-fixture'\nversion='0.0.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            r#"pub fn choose(first: bool, second: bool) -> i32 {
    if first && second { 7 } else { 3 }
}

#[cfg(test)]
mod tests {
    #[test]
    fn unit_choice() {
        assert_eq!(super::choose(true, true), 7);
    }
}
"#,
        )
        .unwrap();
        fs::write(
            root.join("tests/integration.rs"),
            r#"#[test]
fn integration_choice() {
    assert_eq!(rust_project_fixture::choose(false, true), 3);
}
"#,
        )
        .unwrap();
        root
    }

    /// Nothing to read is nothing read: no depinfo line, a rule with no
    /// prerequisite, and a path with no component.
    #[test]
    fn empty_depinfo_and_paths_name_nothing() {
        let mut compiled = BTreeSet::new();
        collect_depinfo_sources("", Path::new("/w"), &mut compiled);
        collect_depinfo_sources("target/x: \n", Path::new("/w"), &mut compiled);
        assert!(compiled.is_empty());
        assert_eq!(normalize(Path::new("")), PathBuf::new());
        assert_eq!(normalize(Path::new("./a/../b")), PathBuf::from("b"));
    }

    #[test]
    fn explicit_roots_narrow_what_rust_instruments() {
        // A module the roots leave out is not instrumented, so it compiles as
        // the author wrote it. The runtime module is still declared in the
        // crate root whether or not that root is kept, which is what lets the
        // modules that are kept refer to it.
        let root = fixture();
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='rust_project_fixture'\nversion='0.0.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "mod helper;\nmod vendored;\npub fn entry(flag: bool) -> i32 { if flag { helper::one() } else { vendored::two() } }\n",
        )
        .unwrap();
        fs::write(root.join("src/helper.rs"), "pub fn one() -> i32 { 1 }\n").unwrap();
        let vendored = "pub fn two() -> i32 { 2 }\n";
        fs::write(root.join("src/vendored.rs"), vendored).unwrap();

        let roots = crate::source_discovery::ExplicitSourceRoots::resolve(
            &root,
            &["src/lib.rs".to_owned(), "src/helper.rs".to_owned()],
        )
        .unwrap();
        let prepared = prepare_rust_project(&root, Some(&roots)).unwrap();
        assert_eq!(prepared.source_files, ["src/helper.rs", "src/lib.rs"]);
        assert!(
            prepared
                .manifest
                .points
                .iter()
                .all(|point| point.file != "src/vendored.rs"),
            "a module outside the roots carries no obligations"
        );
        assert_eq!(
            fs::read_to_string(root.join("src/vendored.rs")).unwrap(),
            vendored,
            "and is left exactly as written"
        );
        let scope: crate::source_discovery::SourceScope =
            serde_json::from_value(prepared.manifest.scope.clone().expect("a scope")).unwrap();
        assert_eq!(
            scope.mode,
            crate::source_discovery::SourceScopeMode::Explicit
        );
        assert!(
            scope
                .entries
                .iter()
                .any(|entry| entry.file == "src/vendored.rs"
                    && entry.reason == "outside explicit source roots"),
            "{scope:?}"
        );
        fs::remove_dir_all(root).ok();
    }

    /// A Rust Worker's library is instrumented as worker-build compiles it
    /// -- for wasm, a release, with its default features -- whatever target
    /// the command builds the rest for, and its crate is known as a Worker's.
    #[test]
    fn a_workers_library_is_instrumented_as_its_worker_build_compiles_it() {
        let root = fixture();
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers = [\"worker\", \"host\"]\nresolver = \"2\"\n",
        )
        .unwrap();
        for directory in ["worker/src", "host/src"] {
            fs::create_dir_all(root.join(directory)).unwrap();
        }
        fs::write(
            root.join("worker/Cargo.toml"),
            "[package]\nname = \"worker\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[lib]\ncrate-type = [\"cdylib\"]\n\n[features]\ndefault = [\"audit\"]\naudit = []\n",
        )
        .unwrap();
        fs::write(
            root.join("worker/wrangler.toml"),
            "main = \"build/worker/shim.mjs\"\n[build]\ncommand = \"worker-build --release\"\n",
        )
        .unwrap();
        let worker = "#[cfg(target_arch = \"wasm32\")]\npub fn runtime() -> u8 {\n    1\n}\n\n#[cfg(not(target_arch = \"wasm32\"))]\npub fn runtime() -> u8 {\n    2\n}\n\n#[cfg(feature = \"audit\")]\npub fn audit() -> u8 {\n    3\n}\n\n#[cfg(debug_assertions)]\npub fn debug() -> u8 {\n    4\n}\n\n#[cfg(custom)]\npub fn custom() -> u8 {\n    5\n}\n";
        fs::write(root.join("worker/src/lib.rs"), worker).unwrap();
        // Its integration tests are built by `cargo test`, not worker-build.
        fs::create_dir_all(root.join("worker/tests")).unwrap();
        fs::write(
            root.join("worker/tests/smoke.rs"),
            "#[test]\nfn smoke() {}\n",
        )
        .unwrap();
        fs::write(
            root.join("host/Cargo.toml"),
            "[package]\nname = \"host\"\nversion = \"0.0.0\"\nedition = \"2024\"\n",
        )
        .unwrap();
        fs::write(
            root.join("host/src/lib.rs"),
            "#[cfg(target_arch = \"wasm32\")]\npub fn wasm() -> u8 {\n    6\n}\n\npub fn host() -> u8 {\n    7\n}\n",
        )
        .unwrap();
        let native =
            crate::rust_instrumenter::TargetCfg::new(&["target_arch=\"x86_64\"".parse().unwrap()]);
        let prepared = prepare_rust_project_for(&root, None, Some(&native)).unwrap();
        let functions = |file: &str| {
            prepared
                .manifest
                .points
                .iter()
                .filter(|point| {
                    point.file == file
                        && point.kind == crate::coverage_analysis::PointKind::Function
                })
                .map(|point| (point.label.clone().unwrap_or_default(), point.line))
                .collect::<BTreeMap<_, _>>()
        };
        // wasm's `runtime` (the first) and the default feature's `audit` are
        // the Worker's; the native `runtime` and the debug build's `debug`
        // are not, and a custom cfg is left for its build to answer.
        let worker = functions("worker/src/lib.rs");
        assert_eq!(
            worker.keys().collect::<Vec<_>>(),
            ["audit", "custom", "runtime"]
        );
        assert!(worker["runtime"] < 6, "{worker:?}");
        assert_eq!(
            prepared
                .probes
                .cfgs
                .iter()
                .map(|gate| (gate.file.as_str(), gate.predicate.as_str()))
                .collect::<Vec<_>>(),
            [("worker/src/lib.rs", "custom")]
        );
        assert_eq!(
            functions("host/src/lib.rs").keys().collect::<Vec<_>>(),
            ["host"]
        );
        let workers = prepared.probes.workers.iter().collect::<Vec<_>>();
        assert_eq!(workers.len(), 1);
        assert_eq!(prepared.probes.crates[workers[0]], ["worker/src/lib.rs"]);
        fs::remove_dir_all(root).ok();
    }

    #[test]
    fn test_code_is_instrumented_for_its_assertions_alone() {
        // An integration test, a `#[cfg(test)]` module inline and one in its
        // own file, named like any other: none of it is measured, as
        // the usual Rust coverage tools leave it out. Each assertion still
        // says where it stood.
        let root = fixture();
        let library = fs::read_to_string(root.join("src/lib.rs")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            format!("#[cfg(test)]\nmod helpers;\n{library}"),
        )
        .unwrap();
        fs::write(
            root.join("src/helpers.rs"),
            "pub fn twice(value: i32) -> i32 {\n    let doubled = value * 2;\n    assert!(doubled >= value);\n    doubled\n}\n",
        )
        .unwrap();

        let prepared = prepare_rust_project(&root, None).unwrap();
        assert_eq!(
            prepared.source_files,
            ["src/helpers.rs", "src/lib.rs", "tests/integration.rs"]
        );
        let manifest = &prepared.manifest;
        let measured = manifest
            .points
            .iter()
            .map(|point| (point.file.as_str(), point.line))
            .chain(
                manifest
                    .decisions
                    .iter()
                    .map(|decision| (decision.file.as_str(), decision.line)),
            )
            .chain(
                manifest
                    .branches
                    .iter()
                    .map(|branch| (branch.file.as_str(), branch.line)),
            )
            .collect::<BTreeSet<_>>();
        // `choose` alone: its function, its statement, its decision.
        assert!(
            measured
                .iter()
                .all(|(file, line)| *file == "src/lib.rs" && (3..=5).contains(line)),
            "{measured:?}"
        );
        assert!(!measured.is_empty());
        let sites = manifest
            .assertion_sites
            .iter()
            .map(|site| (site.file.as_str(), site.source.as_str()))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            sites,
            BTreeSet::from([
                ("src/helpers.rs", "assert!(doubled >= value);"),
                ("src/lib.rs", "assert_eq!(super::choose(true, true), 7);"),
                (
                    "tests/integration.rs",
                    "assert_eq!(rust_project_fixture::choose(false, true), 3);"
                ),
            ])
        );
        // Test code keeps its markers and nothing else.
        let helpers = fs::read_to_string(root.join("src/helpers.rs")).unwrap();
        assert!(helpers.contains("::assertion("), "{helpers}");
        assert!(!helpers.contains("::hit("), "{helpers}");
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn a_proc_macro_crates_own_code_is_declined() {
        // async-trait, serde_derive and thiserror-impl compile to compiler
        // plugins: rustc loads them while building the crate under test and
        // runs them there, so no test process executes a line of them and
        // every file read 0%.
        let root = fixture();
        fs::write(
            root.join("Cargo.toml"),
            concat!(
                "[package]\nname='rust_project_fixture'\nversion='0.0.0'\nedition='2024'\n",
                "\n[lib]\nproc-macro=true\n",
            ),
        )
        .unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "mod helper;\npub fn entry(flag: bool) -> i32 { if flag { helper::one() } else { 0 } }\n",
        )
        .unwrap();
        fs::write(root.join("src/helper.rs"), "pub fn one() -> i32 { 1 }\n").unwrap();

        let prepared = prepare_rust_project(&root, None).unwrap();
        // The files are still instrumented and still in the manifest; their
        // obligations are declined, and a limitation says why.
        let declined = prepared.manifest.unmeasured.iter().collect::<BTreeSet<_>>();
        assert!(!declined.is_empty());
        for point in &prepared.manifest.points {
            let plugin = point.file == "src/lib.rs" || point.file == "src/helper.rs";
            assert_eq!(declined.contains(&point.id), plugin, "{}", point.file);
        }
        let reasons = prepared
            .manifest
            .limitations
            .iter()
            .filter_map(|limitation| limitation.get("id")?.as_str())
            .filter(|id| id.starts_with("rust-proc-macro-runs-in-the-compiler#"))
            .collect::<BTreeSet<_>>();
        assert_eq!(
            reasons,
            BTreeSet::from([
                "rust-proc-macro-runs-in-the-compiler#src/helper.rs",
                "rust-proc-macro-runs-in-the-compiler#src/lib.rs",
            ])
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn modules_declared_inside_a_macro_are_instrumented() {
        // hashbrown selects its SIMD group with `cfg_select! { ... mod neon;
        // ... }`, and `cfg_if!` has the same shape; the declaration lives in
        // the macro's token tree, which no item walk reaches. Both arms are
        // followed -- the build decides which one is in the denominator.
        let root = fixture();
        fs::write(
            root.join("src/lib.rs"),
            concat!(
                "macro_rules! select { ($($rest:tt)*) => { $($rest)* } }\n",
                "select! {\n",
                "    #[cfg(target_endian = \"little\")]\n",
                "    mod little;\n",
                "    #[cfg(not(target_endian = \"little\"))]\n",
                "    mod big;\n",
                "    mod inline { pub fn here() -> i32 { 1 } }\n",
                "}\n",
                "pub fn value() -> i32 { inline::here() }\n",
            ),
        )
        .unwrap();
        fs::write(root.join("src/little.rs"), "pub fn v() -> i32 { 1 }\n").unwrap();
        fs::write(root.join("src/big.rs"), "pub fn v() -> i32 { 2 }\n").unwrap();

        let prepared = prepare_rust_project(&root, None).unwrap();
        // Both arms, and never the inline module, which has no file.
        assert_eq!(
            prepared.source_files,
            [
                "src/big.rs",
                "src/lib.rs",
                "src/little.rs",
                "tests/integration.rs"
            ],
            "{:?}",
            prepared.source_files
        );
        fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn only_files_the_module_tree_reaches_are_instrumented() {
        let root = fixture();
        fs::create_dir_all(root.join("src/nested")).unwrap();
        fs::create_dir_all(root.join("src/deep/inner")).unwrap();
        fs::create_dir_all(root.join("runtime-assets")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            concat!(
                "mod util;\n",
                "mod nested;\n",
                "#[path = \"renamed_file.rs\"]\n",
                "mod renamed;\n",
                "mod deep;\n",
                "include!(\"included.rs\");\n",
                "pub const EMBEDDED: &str = include_str!(\"../runtime-assets/embedded.rs\");\n",
                "pub fn choose(first: bool, second: bool) -> i32 {\n",
                "    if first && second { util::seven() } else { nested::three() }\n",
                "}\n",
            ),
        )
        .unwrap();
        fs::write(root.join("src/util.rs"), "pub fn seven() -> i32 { 7 }\n").unwrap();
        fs::write(
            root.join("src/nested/mod.rs"),
            "mod leaf;\npub fn three() -> i32 { leaf::three() }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/nested/leaf.rs"),
            "pub fn three() -> i32 { 3 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/renamed_file.rs"),
            "pub fn renamed() -> i32 { 1 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/deep.rs"),
            "pub mod inner {\n    mod block_child;\n    pub fn deep() -> i32 { block_child::v() }\n}\n",
        )
        .unwrap();
        fs::write(
            root.join("src/deep/inner/block_child.rs"),
            "pub fn v() -> i32 { 9 }\n",
        )
        .unwrap();
        fs::write(
            root.join("src/included.rs"),
            "pub fn included() -> i32 { 2 }\n",
        )
        .unwrap();
        // serde_json's tests reach into src with `#[path = "../src/..."]`.
        fs::write(
            root.join("tests/integration.rs"),
            concat!(
                "#[path = \"../src/util.rs\"]\n",
                "mod util;\n",
                "#[test]\n",
                "fn integration_choice() {\n",
                "    assert_eq!(rust_project_fixture::choose(false, true), 3);\n",
                "    assert_eq!(util::seven(), 7);\n",
                "}\n",
            ),
        )
        .unwrap();
        // Data, not code: embedded verbatim and compiled by a consumer of
        // its own, which would not know any runtime module of ours.
        let embedded = "pub fn standalone() -> i32 { if true { 1 } else { 0 } }\n";
        fs::write(root.join("runtime-assets/embedded.rs"), embedded).unwrap();
        fs::write(
            root.join("src/orphan.rs"),
            "pub fn unreachable_module() {}\n",
        )
        .unwrap();

        let prepared = prepare_rust_project(&root, None).unwrap();
        assert_eq!(
            prepared.source_files,
            [
                "src/deep.rs",
                "src/deep/inner/block_child.rs",
                "src/included.rs",
                "src/lib.rs",
                "src/nested/leaf.rs",
                "src/nested/mod.rs",
                "src/renamed_file.rs",
                "src/util.rs",
                "tests/integration.rs",
            ]
        );
        assert_eq!(
            fs::read_to_string(root.join("runtime-assets/embedded.rs")).unwrap(),
            embedded
        );
        assert!(
            !fs::read_to_string(root.join("src/orphan.rs"))
                .unwrap()
                .contains("__supercov")
        );
        assert!(
            fs::read_to_string(root.join("src/deep/inner/block_child.rs"))
                .unwrap()
                .contains("__supercov")
        );
        let build = Command::new("cargo")
            .args(["test", "--no-run"])
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", &prepared.target_directory)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_module_shared_through_a_symlink_is_instrumented_once() {
        let root = fixture();
        fs::write(root.join("src/shared.rs"), "pub fn shared() -> i32 { 5 }\n").unwrap();
        std::os::unix::fs::symlink("../src/shared.rs", root.join("tests/shared.rs")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            concat!(
                "pub mod shared;\n",
                "pub fn choose(first: bool, second: bool) -> i32 {\n",
                "    if first && second { 7 } else { shared::shared() }\n",
                "}\n",
            ),
        )
        .unwrap();
        fs::write(
            root.join("tests/integration.rs"),
            concat!(
                "mod shared;\n",
                "#[test]\n",
                "fn integration_choice() {\n",
                "    assert_eq!(rust_project_fixture::choose(false, true), 5);\n",
                "    assert_eq!(shared::shared(), 5);\n",
                "}\n",
            ),
        )
        .unwrap();
        let prepared = prepare_rust_project(&root, None).unwrap();
        // The target's path, once; never the symlink's spelling.
        let shared = prepared
            .source_files
            .iter()
            .filter(|file| file.ends_with("shared.rs"))
            .collect::<Vec<_>>();
        assert_eq!(shared, ["src/shared.rs"], "{:?}", prepared.source_files);
        // The one function in it carries one function probe: instrumented
        // once, through whichever spelling reached it first.
        let instrumented = fs::read_to_string(root.join("src/shared.rs")).unwrap();
        let function = prepared
            .manifest
            .points
            .iter()
            .find(|point| point.file == "src/shared.rs" && point.id.starts_with("rs:function:"))
            .expect("the shared function's probe");
        let ordinal = prepared
            .probes
            .hits
            .iter()
            .position(|id| id == &function.id)
            .unwrap();
        assert_eq!(
            instrumented.matches(&format!("::hit({ordinal});")).count(),
            1
        );
        let build = Command::new("cargo")
            .args(["test", "--no-run"])
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", &prepared.target_directory)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn crate_keys_carry_the_manifest_token() {
        let root = fixture();
        let prepared = prepare_rust_project(&root, None).unwrap();
        let token = manifest_token(&prepared.manifest);
        assert_eq!(token.len(), 12);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert_eq!(token, manifest_token(&prepared.manifest));
        let key = crate_key(&token, "src/lib.rs");
        assert_eq!(key.len(), 24);
        assert!(key.starts_with(&token));
        assert_ne!(key, crate_key(&token, "tests/integration.rs"));
        for crate_root in &prepared.crate_roots {
            assert!(
                fs::read_to_string(root.join(crate_root))
                    .unwrap()
                    .contains(&crate_key(&token, crate_root))
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn prepares_every_workspace_crate_root_and_compiles_without_manifest_changes() {
        let root = fixture();
        let manifest_before = fs::read(root.join("Cargo.toml")).unwrap();
        let prepared = prepare_rust_project(&root, None).unwrap();
        assert_eq!(
            prepared.source_files,
            ["src/lib.rs", "tests/integration.rs"]
        );
        assert_eq!(prepared.crate_roots, ["src/lib.rs", "tests/integration.rs"]);
        assert!(!prepared.manifest.points.is_empty());
        assert!(!prepared.manifest.decisions.is_empty());
        assert_eq!(fs::read(root.join("Cargo.toml")).unwrap(), manifest_before);
        for crate_root in &prepared.crate_roots {
            assert!(
                fs::read_to_string(root.join(crate_root))
                    .unwrap()
                    .contains(&format!("mod {}", prepared.runtime_module))
            );
        }
        let build = Command::new("cargo")
            .args(["test", "--no-run"])
            .current_dir(&root)
            .env("CARGO_TARGET_DIR", &prepared.target_directory)
            .output()
            .unwrap();
        assert!(
            build.status.success(),
            "{}",
            String::from_utf8_lossy(&build.stderr)
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A backslash not before a space is part of the path, and the empty
    /// word between two spaces is no path; a path is confined to its root;
    /// and the runtime's module takes the first name no source already uses.
    #[test]
    fn depinfo_paths_confined_paths_and_the_runtime_s_name() {
        let mut compiled = BTreeSet::new();
        collect_depinfo_sources("t: src\\x.rs  src/y.rs\n", Path::new("/w"), &mut compiled);
        // Kept, and written with `/` as every path is.
        assert_eq!(
            compiled.into_iter().collect::<Vec<_>>(),
            ["src/x.rs", "src/y.rs"]
        );
        let root = Path::new("/w");
        assert_eq!(
            confined_relative(root, Path::new("/w/src/a.rs")).unwrap(),
            "src/a.rs"
        );
        for refused in ["/w", "/x/a.rs", "/w/../x"] {
            assert!(
                confined_relative(root, Path::new(refused)).is_err(),
                "{refused}"
            );
        }
        let sources = |texts: &[&str]| {
            texts
                .iter()
                .enumerate()
                .map(|(at, text)| (format!("src/{at}.rs"), (*text).to_owned()))
                .collect::<BTreeMap<_, _>>()
        };
        assert_eq!(
            runtime_module_name(&sources(&["fn a() {}"])),
            "__supercov_runtime_v1"
        );
        assert_eq!(
            runtime_module_name(&sources(&["mod __supercov_runtime_v1;"])),
            "__supercov_runtime_v1_1"
        );
        assert_eq!(
            runtime_module_name(&sources(&[
                "__supercov_runtime_v1",
                "__supercov_runtime_v1_1"
            ])),
            "__supercov_runtime_v1_2"
        );
    }

    /// Depinfo names every source of every rule, a space escaped; only
    /// workspace `.rs` files count, and a directory with no depinfo says
    /// nothing at all.
    #[test]
    fn depinfo_names_the_workspace_sources_each_rule_read() {
        use crate::test_directory::native;
        let root = native("/w");
        let workspace = Path::new(&root);
        let mut compiled = BTreeSet::new();
        collect_depinfo_sources(
            &format!(
                "{}: src/lib.rs src/my\\ mod.rs {} {} build.toml\nsrc/lib.rs:\nnot a rule\n",
                native("/w/target/debug/deps/a.d"),
                native("/w/src/abs.rs"),
                native("/home/.cargo/registry/dep.rs"),
            ),
            workspace,
            &mut compiled,
        );
        assert_eq!(
            compiled.into_iter().collect::<Vec<_>>(),
            ["src/abs.rs", "src/lib.rs", "src/my mod.rs"]
        );
        let root = std::env::temp_dir().join(format!(
            "supercov-depinfo-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("target/debug/deps")).unwrap();
        assert_eq!(compiled_source_files(&root, &root.join("target")), None);
        fs::write(root.join("target/debug/deps/x.d"), "x: src/main.rs\n").unwrap();
        fs::write(root.join("target/debug/deps/x.rlib"), "").unwrap();
        assert_eq!(
            compiled_source_files(&root, &root.join("target")),
            Some(["src/main.rs".to_owned()].into_iter().collect())
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn depinfo_is_read_from_regular_utf8_files_alone() {
        let root = fixture();
        let target = root.join("target/debug/deps");
        fs::create_dir_all(&target).unwrap();
        fs::write(
            target.join("fixture.d"),
            format!(
                "{0}/lib: src/lib.rs {0}/src/with\\ space.rs\n",
                root.display()
            ),
        )
        .unwrap();
        // Not UTF-8, so not depinfo rustc wrote.
        fs::write(target.join("garbled.d"), [0xff, 0xfe, b':']).unwrap();
        // A symlink is neither walked nor read.
        std::os::unix::fs::symlink(target.join("fixture.d"), target.join("linked.d")).unwrap();
        std::os::unix::fs::symlink(&target, root.join("target/loop")).unwrap();
        assert_eq!(
            compiled_source_files(&root, &root.join("target")),
            Some(BTreeSet::from([
                "src/lib.rs".to_owned(),
                "src/with space.rs".to_owned()
            ]))
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// The files a crate compiles, as rustc finds them: `name.rs` and
    /// `name/mod.rs`, `#[path]` from the file and from an inline module,
    /// literal `include!`s and the modules they declare, both arms of a
    /// `cfg_if!`; not a computed include, a path out of the workspace or a
    /// module whose file is missing. What only tests reach is test code.
    #[test]
    fn the_module_tree_is_resolved_the_way_rustc_resolves_it() {
        let base = std::env::temp_dir().join(format!(
            "supercov-module-tree-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let workspace = base.join("w");
        for (file, text) in [
            (
                "src/lib.rs",
                "mod a;\nmod b;\n#[path = \"other/c_impl.rs\"]\nmod c;\nmod inline {\n    #[path = \"deep.rs\"]\n    mod d;\n    mod e;\n}\n#[cfg(test)]\nmod tests;\ninclude!(\"gen.rs\");\ninclude!(concat!(env!(\"OUT_DIR\"), \"/x.rs\"));\nconst DATA: &str = include_str!(\"data.txt\");\n#[path = \"../../outside.rs\"]\nmod out;\ncfg_if::cfg_if! {\n    if #[cfg(unix)] {\n        mod unix_impl;\n    } else {\n        mod other_impl;\n    }\n}\nmod missing;\nmod ;\nquirk! { mod 3; }\ninclude!(GENERATED);\n",
            ),
            ("src/a.rs", "mod a_child;\n"),
            ("src/a/a_child.rs", ""),
            ("src/b/mod.rs", ""),
            ("src/other/c_impl.rs", ""),
            ("src/inline/deep.rs", ""),
            ("src/inline/e.rs", ""),
            ("src/tests.rs", ""),
            ("src/gen.rs", "mod generated_child;\n"),
            ("src/generated_child.rs", ""),
            ("src/unix_impl.rs", ""),
            ("src/data.txt", ""),
            ("tests/it.rs", "mod common;\n"),
            ("tests/common/mod.rs", ""),
            ("../outside.rs", ""),
        ] {
            let path = workspace.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
        let roots = [workspace.join("src/lib.rs"), workspace.join("tests/it.rs")]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let test_roots = [workspace.join("tests/it.rs")]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let (mut files, mut tests, mut owners) =
            (BTreeSet::new(), BTreeSet::new(), BTreeMap::new());
        resolve_module_tree(
            &workspace,
            &roots,
            &test_roots,
            &mut files,
            &mut tests,
            &mut owners,
        )
        .unwrap();
        let names = |set: &BTreeSet<PathBuf>| {
            set.iter()
                .map(|p| {
                    p.strip_prefix(&workspace)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/")
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            names(&files),
            [
                "src/a/a_child.rs",
                "src/a.rs",
                "src/b/mod.rs",
                "src/gen.rs",
                "src/generated_child.rs",
                "src/inline/deep.rs",
                "src/inline/e.rs",
                "src/lib.rs",
                "src/other/c_impl.rs",
                "src/tests.rs",
                "src/unix_impl.rs",
                "tests/common/mod.rs",
                "tests/it.rs",
            ]
        );
        assert_eq!(
            names(&tests),
            ["src/tests.rs", "tests/common/mod.rs", "tests/it.rs"]
        );
        assert_eq!(
            owners[&workspace.join("src/a.rs")],
            [workspace.join("src/lib.rs")].into_iter().collect()
        );
        fs::remove_dir_all(base).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_module_must_be_a_file_inside_the_workspace() {
        let outside = fixture();
        for target in [outside.join("src/lib.rs"), PathBuf::from("../tests")] {
            let root = fixture();
            fs::write(root.join("src/lib.rs"), "mod linked;\n").unwrap();
            std::os::unix::fs::symlink(&target, root.join("src/linked.rs")).unwrap();
            let error = prepare_rust_project(&root, None).unwrap_err();
            assert!(
                matches!(&error, RustProjectError::UnsafePath(path) if path.ends_with("src/linked.rs")),
                "{target:?}: {error}"
            );
            fs::remove_dir_all(root).unwrap();
        }
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn the_module_tree_classifies_what_it_reaches() {
        let root = fixture();
        fs::write(root.join("build.rs"), "fn main() {}\n").unwrap();
        fs::create_dir_all(root.join("src/checks")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            concat!(
                // One file, two modules: walked once for the crate.
                "#[path = \"shared.rs\"] pub mod first;\n",
                "#[path = \"shared.rs\"] pub mod second;\n",
                // Named as test code, though no `cfg(test)` gates it.
                "mod tests;\n",
                // Test code, by the gate on the module that declares it.
                "#[cfg(test)]\nmod checks {\n    mod support;\n}\n",
            ),
        )
        .unwrap();
        fs::write(root.join("src/shared.rs"), "pub fn shared() -> i32 { 5 }\n").unwrap();
        fs::write(root.join("src/tests.rs"), "fn helper() -> i32 { 1 }\n").unwrap();
        fs::write(
            root.join("src/checks/support.rs"),
            "fn support() -> i32 { 2 }\n",
        )
        .unwrap();
        let prepared = prepare_rust_project(&root, None).unwrap();
        // A build script is compiled and run on its own, never by a test.
        assert_eq!(
            prepared.source_files,
            [
                "src/checks/support.rs",
                "src/lib.rs",
                "src/shared.rs",
                "src/tests.rs",
                "tests/integration.rs"
            ]
        );
        let measured = prepared
            .manifest
            .points
            .iter()
            .map(|point| point.file.as_str())
            .collect::<BTreeSet<_>>();
        // `src/tests.rs` and `src/checks/support.rs` are test code: their
        // functions carry no probes.
        assert_eq!(measured, BTreeSet::from(["src/shared.rs"]));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_workspace_cargo_cannot_describe_as_one_with_sources_is_refused() {
        let root = fixture();
        fs::write(root.join("Cargo.toml"), "[package\n").unwrap();
        assert!(matches!(
            prepare_rust_project(&root, None),
            Err(RustProjectError::MetadataFailed(_))
        ));
        // A member is not the workspace Cargo reports.
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=['member']\nresolver='3'\n",
        )
        .unwrap();
        fs::create_dir_all(root.join("member/src")).unwrap();
        fs::write(
            root.join("member/Cargo.toml"),
            "[package]\nname='member'\nversion='0.0.0'\nedition='2024'\n",
        )
        .unwrap();
        fs::write(root.join("member/src/lib.rs"), "").unwrap();
        // Compared canonical: macOS's temporary directory is reached through
        // a link (`/var` is `/private/var`), and Cargo names the real path.
        assert!(matches!(
            prepare_rust_project(&root.join("member"), None),
            Err(RustProjectError::UnsafePath(path))
                if Path::new(&path).canonicalize().ok() == root.canonicalize().ok()
        ));
        fs::write(
            root.join("Cargo.toml"),
            "[workspace]\nmembers=[]\nresolver='3'\n",
        )
        .unwrap();
        assert!(matches!(
            prepare_rust_project(&root, None),
            Err(RustProjectError::NoWorkspacePackages)
        ));
        // A library whose path is a directory has no source to read.
        fs::write(
            root.join("Cargo.toml"),
            "[package]\nname='fixture'\nversion='0.0.0'\nedition='2024'\nautotests=false\n[lib]\npath='src'\n",
        )
        .unwrap();
        assert!(matches!(
            prepare_rust_project(&root, None),
            Err(RustProjectError::NoSourceFiles)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn merging_refuses_a_repeated_obligation_and_keeps_one_limitation_per_site() {
        let instrumented = || {
            instrument_rust_file_numbered(
                "src/lib.rs",
                "pub fn one(flag: bool) -> i32 { if flag { 1 } else { 0 } }\n",
                "crate::__supercov_runtime_v1",
                false,
                None,
                &mut crate::rust_instrumenter::RustProbeLayout::default(),
            )
            .unwrap()
            .manifest
        };
        let mut destination = instrumented();
        let limitation =
            serde_json::json!({"id": "note", "file": "src/lib.rs", "line": 1, "column": 0});
        destination.limitations.push(limitation.clone());
        let mut source = CoverageManifest {
            points: Vec::new(),
            decisions: Vec::new(),
            branches: Vec::new(),
            ..instrumented()
        };
        source.limitations.push(limitation.clone());
        merge_manifest(&mut destination, source).unwrap();
        assert_eq!(
            destination
                .limitations
                .iter()
                .filter(|existing| **existing == limitation)
                .count(),
            1
        );
        assert!(matches!(
            merge_manifest(&mut destination, instrumented()),
            Err(RustProjectError::DuplicateObligation(_))
        ));
    }
}
