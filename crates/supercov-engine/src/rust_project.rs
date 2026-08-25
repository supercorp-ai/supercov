//! Cargo workspace discovery and isolated owned-Rust frontend preparation.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Component, Path, PathBuf},
    process::Command,
};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::{
    coverage_report::CoverageManifest, rust_instrumenter::instrument_rust_source,
    rust_runtime::render_rust_runtime,
};

#[derive(Debug, Clone, PartialEq)]
pub struct PreparedRustProject {
    pub workspace_root: PathBuf,
    pub target_directory: PathBuf,
    pub source_files: Vec<String>,
    pub crate_roots: Vec<String>,
    pub runtime_module: String,
    pub manifest: CoverageManifest,
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
    Runtime(String),
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
            Self::Runtime(reason) => write!(formatter, "could not generate Rust runtime: {reason}"),
        }
    }
}

impl std::error::Error for RustProjectError {}

#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
    workspace_members: Vec<String>,
    workspace_root: PathBuf,
    target_directory: PathBuf,
}

#[derive(Deserialize)]
struct CargoPackage {
    id: String,
    manifest_path: PathBuf,
    targets: Vec<CargoTarget>,
}

#[derive(Deserialize)]
struct CargoTarget {
    kind: Vec<String>,
    src_path: PathBuf,
}

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
    let target_directory = root.join(".supercov/rust-target");
    let output = Command::new("cargo")
        .args(["metadata", "--format-version=1", "--no-deps"])
        .current_dir(root)
        .env("CARGO_TARGET_DIR", &target_directory)
        .output()
        .map_err(|error| RustProjectError::MetadataLaunch(error.to_string()))?;
    if !output.status.success() {
        return Err(RustProjectError::MetadataFailed(
            String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| RustProjectError::MetadataJson(error.to_string()))
}

fn collect_rust_files(
    directory: &Path,
    files: &mut BTreeSet<PathBuf>,
) -> Result<(), RustProjectError> {
    let mut entries = fs::read_dir(directory)
        .map_err(|error| RustProjectError::Io {
            path: directory.to_owned(),
            reason: error.to_string(),
        })?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| RustProjectError::Io {
            path: directory.to_owned(),
            reason: error.to_string(),
        })?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if matches!(name.as_ref(), ".git" | ".supercov" | "target") {
            continue;
        }
        let file_type = entry.file_type().map_err(|error| RustProjectError::Io {
            path: entry.path(),
            reason: error.to_string(),
        })?;
        if file_type.is_symlink() {
            return Err(RustProjectError::UnsafePath(
                entry.path().display().to_string(),
            ));
        }
        if file_type.is_dir() {
            collect_rust_files(&entry.path(), files)?;
        } else if file_type.is_file()
            && entry.path().extension().and_then(|value| value.to_str()) == Some("rs")
            && entry.file_name() != "build.rs"
        {
            files.insert(entry.path());
        }
    }
    Ok(())
}

/// Read-only Cargo workspace source discovery used by integrity checks. This
/// deliberately shares the same path policy as transformation preparation.
pub fn discover_rust_source_files(workspace: &Path) -> Result<Vec<String>, RustProjectError> {
    let workspace = canonical_directory(workspace)?;
    let metadata = cargo_metadata(&workspace)?;
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
    let mut files = BTreeSet::new();
    for package in packages {
        let directory = package.manifest_path.parent().ok_or_else(|| {
            RustProjectError::UnsafePath(package.manifest_path.display().to_string())
        })?;
        let directory = canonical_directory(directory)?;
        confined_relative(&workspace, &directory).or_else(|error| {
            (directory == workspace)
                .then_some(String::new())
                .ok_or(error)
        })?;
        collect_rust_files(&directory, &mut files)?;
    }
    if files.is_empty() {
        return Err(RustProjectError::NoSourceFiles);
    }
    files
        .into_iter()
        .map(|path| confined_relative(&workspace, &path))
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

fn crate_key(path: &str) -> String {
    let digest = Sha256::digest(path.as_bytes());
    digest[..12]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
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
    destination.decisions.append(&mut source.decisions);
    destination.branches.append(&mut source.branches);
    for limitation in source.limitations {
        let id = limitation.get("id").and_then(|value| value.as_str());
        if !destination
            .limitations
            .iter()
            .any(|existing| existing.get("id").and_then(|value| value.as_str()) == id)
        {
            destination.limitations.push(limitation);
        }
    }
    Ok(())
}

pub fn prepare_rust_project(workspace: &Path) -> Result<PreparedRustProject, RustProjectError> {
    let workspace = canonical_directory(workspace)?;
    let metadata = cargo_metadata(&workspace)?;
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

    let mut files = BTreeSet::new();
    let mut roots = BTreeSet::new();
    for package in &packages {
        let directory = package.manifest_path.parent().ok_or_else(|| {
            RustProjectError::UnsafePath(package.manifest_path.display().to_string())
        })?;
        let directory = canonical_directory(directory)?;
        confined_relative(&workspace, &directory).or_else(|error| {
            (directory == workspace)
                .then_some(String::new())
                .ok_or(error)
        })?;
        collect_rust_files(&directory, &mut files)?;
        for target in &package.targets {
            if target.kind.iter().any(|kind| kind == "custom-build") {
                continue;
            }
            let root =
                fs::canonicalize(&target.src_path).map_err(|error| RustProjectError::Io {
                    path: target.src_path.clone(),
                    reason: error.to_string(),
                })?;
            confined_relative(&workspace, &root)?;
            roots.insert(root);
        }
    }
    if files.is_empty() {
        return Err(RustProjectError::NoSourceFiles);
    }

    let mut sources = BTreeMap::new();
    for path in files {
        let relative = confined_relative(&workspace, &path)?;
        let source = fs::read_to_string(&path).map_err(|error| RustProjectError::Io {
            path: path.clone(),
            reason: error.to_string(),
        })?;
        sources.insert(relative, source);
    }
    let runtime_module = runtime_module_name(&sources);
    let runtime_path = format!("crate::{runtime_module}");
    let mut manifest = CoverageManifest {
        decisions: Vec::new(),
        points: Vec::new(),
        branches: Vec::new(),
        limitations: Vec::new(),
        scope: None,
    };
    for (relative, source) in &sources {
        let transformed =
            instrument_rust_source(relative, source, &runtime_path).map_err(|error| {
                RustProjectError::Instrument {
                    file: relative.clone(),
                    reason: error.to_string(),
                }
            })?;
        merge_manifest(&mut manifest, transformed.manifest)?;
        fs::write(workspace.join(relative), transformed.code).map_err(|error| {
            RustProjectError::Io {
                path: workspace.join(relative),
                reason: error.to_string(),
            }
        })?;
    }

    let mut crate_roots = Vec::new();
    for root in roots {
        let relative = confined_relative(&workspace, &root)?;
        let runtime = render_rust_runtime(&runtime_module, &crate_key(&relative))
            .map_err(RustProjectError::Runtime)?;
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
    }

    manifest
        .points
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
    let target_directory = metadata.target_directory;
    let target_directory = if target_directory.is_absolute() {
        target_directory
    } else {
        workspace.join(target_directory)
    };
    if !target_directory.starts_with(&workspace) {
        return Err(RustProjectError::UnsafePath(
            target_directory.display().to_string(),
        ));
    }
    Ok(PreparedRustProject {
        workspace_root: workspace,
        target_directory,
        source_files: sources.into_keys().collect(),
        crate_roots,
        runtime_module,
        manifest,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        process::Command,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    fn fixture() -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-project-{}-{nonce}",
            std::process::id()
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

    #[test]
    fn prepares_every_workspace_crate_root_and_compiles_without_manifest_changes() {
        let root = fixture();
        let manifest_before = fs::read(root.join("Cargo.toml")).unwrap();
        let prepared = prepare_rust_project(&root).unwrap();
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
}
