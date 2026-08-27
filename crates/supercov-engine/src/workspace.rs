//! Isolated project snapshots and crash-recoverable stable build cache.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lifecycle::{LifecycleError, ProjectLock, atomic_rename, remove_stored_tree_deferred};

const WORKSPACE_MARKER: &str = ".supercov-workspace-store";
const CARGO_WORKSPACE_VERSION: u32 = 1;
const ROOT_EXCLUSIONS: &[&str] = &[
    ".cache",
    ".git",
    ".supercov",
    ".mcdc-pool",
    "node_modules",
    "build",
    "dist",
    ".next",
    ".nuxt",
    ".output",
    "coverage",
    "playwright-report",
    "test-results",
    "target",
];
const NESTED_EXCLUSIONS: &[&str] = &[".supercov", ".mcdc-pool"];
static UNIQUE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum WorkspaceError {
    Io { path: PathBuf, source: io::Error },
    UnsafePath(PathBuf),
    EscapingLink { path: PathBuf, target: PathBuf },
    UnsupportedEntry(PathBuf),
    MissingLock,
    Lifecycle(LifecycleError),
    InvalidCacheMetadata(serde_json::Error),
    UnsupportedPlatform(&'static str),
}

impl std::fmt::Display for WorkspaceError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::UnsafePath(path) => {
                write!(formatter, "unsafe workspace path: {}", path.display())
            }
            Self::EscapingLink { path, target } => write!(
                formatter,
                "refusing to preserve symlink outside the isolated project: {} -> {}",
                path.display(),
                target.display()
            ),
            Self::UnsupportedEntry(path) => {
                write!(
                    formatter,
                    "unsupported filesystem entry in isolated project: {}",
                    path.display()
                )
            }
            Self::MissingLock => write!(
                formatter,
                "isolated workspace preparation requires the active project lock"
            ),
            Self::Lifecycle(error) => write!(formatter, "{error}"),
            Self::InvalidCacheMetadata(error) => {
                write!(formatter, "invalid build-cache metadata: {error}")
            }
            Self::UnsupportedPlatform(reason) => {
                write!(formatter, "unsupported workspace platform: {reason}")
            }
        }
    }
}

impl std::error::Error for WorkspaceError {}

impl From<LifecycleError> for WorkspaceError {
    fn from(value: LifecycleError) -> Self {
        Self::Lifecycle(value)
    }
}

fn io_error(path: &Path, source: io::Error) -> WorkspaceError {
    WorkspaceError::Io {
        path: path.to_owned(),
        source,
    }
}

fn unique() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!(
        "{}-{nanos}-{}",
        std::process::id(),
        UNIQUE.fetch_add(1, Ordering::Relaxed)
    )
}

fn project_name(root: &Path) -> Result<&std::ffi::OsStr, WorkspaceError> {
    root.file_name()
        .ok_or_else(|| WorkspaceError::UnsafePath(root.into()))
}

pub fn workspace_container(root: &Path) -> PathBuf {
    root.join(".supercov/cache")
}

pub fn cached_workspace_path(root: &Path) -> Result<PathBuf, WorkspaceError> {
    Ok(workspace_container(root)
        .join("workspace")
        .join(project_name(root)?))
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CargoWorkspaceMarker {
    version: u32,
    root_sha256: String,
}

fn canonical_root_and_digest(root: &Path) -> Result<(PathBuf, String), WorkspaceError> {
    let canonical = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    let digest = format!(
        "{:x}",
        Sha256::digest(canonical.as_os_str().as_encoded_bytes())
    );
    Ok((canonical, digest))
}

pub fn cargo_workspace_container(root: &Path) -> Result<PathBuf, WorkspaceError> {
    let (canonical, digest) = canonical_root_and_digest(root)?;
    let parent = canonical
        .parent()
        .ok_or_else(|| WorkspaceError::UnsafePath(canonical.clone()))?;
    Ok(parent.join(format!(".supercov-cargo-{}", &digest[..24])))
}

pub fn cargo_cached_workspace_path(root: &Path) -> Result<PathBuf, WorkspaceError> {
    let container = cargo_workspace_container(root)?;
    Ok(container.join("workspace/root").join(project_name(root)?))
}

fn expected_cargo_marker(root: &Path) -> Result<CargoWorkspaceMarker, WorkspaceError> {
    let (_, digest) = canonical_root_and_digest(root)?;
    Ok(CargoWorkspaceMarker {
        version: CARGO_WORKSPACE_VERSION,
        root_sha256: digest,
    })
}

fn read_cargo_marker(path: &Path) -> Result<CargoWorkspaceMarker, WorkspaceError> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error(path, error))?;
    if !metadata.file_type().is_file() {
        return Err(WorkspaceError::UnsafePath(path.into()));
    }
    serde_json::from_slice(&fs::read(path).map_err(|error| io_error(path, error))?)
        .map_err(WorkspaceError::InvalidCacheMetadata)
}

fn ensure_cargo_container(root: &Path) -> Result<PathBuf, WorkspaceError> {
    let container = cargo_workspace_container(root)?;
    let expected = expected_cargo_marker(root)?;
    let mut created = false;
    match fs::symlink_metadata(&container) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(WorkspaceError::UnsafePath(container));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir(&container).map_err(|source| io_error(&container, source))?;
            created = true;
        }
        Err(source) => return Err(io_error(&container, source)),
    }
    let marker_path = container.join(WORKSPACE_MARKER);
    let result = (|| {
        match fs::symlink_metadata(&marker_path) {
            Ok(_) if read_cargo_marker(&marker_path)? != expected => {
                return Err(WorkspaceError::UnsafePath(container.clone()));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound && created => {
                let mut bytes = serde_json::to_vec_pretty(&expected)
                    .map_err(WorkspaceError::InvalidCacheMetadata)?;
                bytes.push(b'\n');
                let mut file = OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(&marker_path)
                    .map_err(|source| io_error(&marker_path, source))?;
                file.write_all(&bytes)
                    .and_then(|_| file.sync_all())
                    .map_err(|source| io_error(&marker_path, source))?;
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Err(WorkspaceError::UnsafePath(container.clone()));
            }
            Err(source) => return Err(io_error(&marker_path, source)),
        }
        for path in [container.join(".cargo"), container.join("workspace/.cargo")] {
            if fs::symlink_metadata(&path).is_ok() {
                return Err(WorkspaceError::UnsafePath(path));
            }
        }
        if project_name(root)? != ".cargo" {
            let path = container.join("workspace/root/.cargo");
            if fs::symlink_metadata(&path).is_ok() {
                return Err(WorkspaceError::UnsafePath(path));
            }
        }
        Ok(())
    })();
    if result.is_err() && created {
        let _ = fs::remove_dir_all(&container);
    }
    result.map(|()| container)
}

pub fn isolated_workspace_path(root: &Path, run_id: &str) -> Result<PathBuf, WorkspaceError> {
    if run_id.is_empty()
        || run_id == "."
        || run_id == ".."
        || run_id
            .chars()
            .any(|character| matches!(character, '/' | '\\' | '\0') || character.is_control())
    {
        return Err(WorkspaceError::UnsafePath(PathBuf::from(run_id)));
    }
    Ok(root
        .join(".supercov/work")
        .join(run_id)
        .join(project_name(root)?))
}

fn marker_owned(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
        && fs::symlink_metadata(path.join(WORKSPACE_MARKER))
            .is_ok_and(|metadata| metadata.file_type().is_file())
}

fn ensure_container(root: &Path) -> Result<PathBuf, WorkspaceError> {
    let container = workspace_container(root);
    match fs::symlink_metadata(&container) {
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(WorkspaceError::UnsafePath(container));
        }
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            fs::create_dir_all(&container).map_err(|source| io_error(&container, source))?;
        }
        Err(source) => return Err(io_error(&container, source)),
    }
    for (name, contents) in [
        (".gitignore", "*\n"),
        (
            WORKSPACE_MARKER,
            "Supercov instrumented workspace. Safe to delete.\n",
        ),
    ] {
        let path = container.join(name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(mut file) => file
                .write_all(contents.as_bytes())
                .and_then(|_| file.sync_all())
                .map_err(|source| io_error(&path, source))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file())
                {
                    return Err(WorkspaceError::UnsafePath(path));
                }
            }
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    Ok(container)
}

fn lexical_normalize(path: &Path) -> Option<PathBuf> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(value) => normalized.push(value),
        }
    }
    Some(normalized)
}

fn inside(root: &Path, path: &Path) -> bool {
    path == root
        || path.strip_prefix(root).is_ok_and(|local| {
            !local.as_os_str().is_empty()
                && local
                    .components()
                    .all(|component| matches!(component, Component::Normal(_)))
        })
}

trait WorkspaceOperations {
    fn copy_file(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError>;
    fn rename(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError>;
}

struct SystemWorkspaceOperations;

impl WorkspaceOperations for SystemWorkspaceOperations {
    fn copy_file(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
        // Per-file APFS clonefile calls can cost tens of milliseconds for tiny
        // files—far more than copying their bytes. Preserve CoW for genuinely
        // large artifacts, while ordinary source/config files take the fast
        // portable path.
        const REFLINK_MINIMUM_BYTES: u64 = 1024 * 1024;
        let bytes = fs::symlink_metadata(source)
            .map_err(|source_error| io_error(source, source_error))?
            .len();
        if bytes < REFLINK_MINIMUM_BYTES {
            fs::copy(source, destination)
                .map(|_| ())
                .map_err(|source_error| io_error(destination, source_error))
        } else {
            reflink_copy::reflink_or_copy(source, destination)
                .map(|_| ())
                .map_err(|source_error| io_error(destination, source_error))
        }
    }

    fn rename(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
        atomic_rename(source, destination).map_err(WorkspaceError::from)
    }
}

#[cfg(unix)]
fn create_link(target: &Path, destination: &Path, _directory: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(windows)]
fn create_link(target: &Path, destination: &Path, directory: bool) -> io::Result<()> {
    if directory {
        // Junctions work on ordinary NTFS installations without requiring
        // Developer Mode or SeCreateSymbolicLinkPrivilege. This is the common
        // path for dependency mounts and internal directory links. A project
        // may still live on a non-NTFS/network filesystem where junctions are
        // unavailable but unprivileged symlinks are enabled, so preserve that
        // valid fallback instead of assuming one Windows filesystem.
        match junction::create(target, destination) {
            Ok(()) => Ok(()),
            Err(junction_error) => {
                let _ = fs::remove_dir(destination);
                std::os::windows::fs::symlink_dir(target, destination).map_err(
                    |symlink_error| {
                        io::Error::new(
                            symlink_error.kind(),
                            format!(
                                "junction creation failed ({junction_error}); directory symlink fallback failed ({symlink_error})"
                            ),
                        )
                    },
                )
            }
        }
    } else {
        // A project that already contains a file symlink necessarily runs in
        // an environment capable of creating one. Do not silently copy it:
        // that can change Node realpath/module-identity semantics.
        std::os::windows::fs::symlink_file(target, destination)
    }
}

#[derive(Clone, Copy)]
struct CopyRoots<'a> {
    source: &'a Path,
    destination: &'a Path,
    final_destination: &'a Path,
    canonical_source: &'a Path,
}

fn copy_tree<Operations: WorkspaceOperations>(
    source: &Path,
    destination: &Path,
    roots: CopyRoots<'_>,
    root_level: bool,
    operations: &mut Operations,
) -> Result<(), WorkspaceError> {
    fs::create_dir_all(destination).map_err(|source| io_error(destination, source))?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| io_error(source, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(source, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let name = entry.file_name();
        let name_text = name
            .to_str()
            .ok_or_else(|| WorkspaceError::UnsafePath(entry.path()))?;
        if (root_level && ROOT_EXCLUSIONS.contains(&name_text))
            || NESTED_EXCLUSIONS.contains(&name_text)
        {
            continue;
        }
        let from = entry.path();
        let metadata = fs::symlink_metadata(&from).map_err(|error| io_error(&from, error))?;
        if metadata.file_type().is_dir() && marker_owned(&from) {
            continue;
        }
        let to = destination.join(&name);
        if metadata.file_type().is_dir() {
            copy_tree(&from, &to, roots, false, operations)?;
        } else if metadata.file_type().is_symlink() {
            let link = fs::read_link(&from).map_err(|error| io_error(&from, error))?;
            let unresolved_target = if link.is_absolute() {
                link.clone()
            } else {
                from.parent().expect("entry parent").join(&link)
            };
            let lexical_target = lexical_normalize(&unresolved_target).ok_or_else(|| {
                WorkspaceError::EscapingLink {
                    path: from.clone(),
                    target: link.clone(),
                }
            })?;
            let canonical_target =
                fs::canonicalize(&from).map_err(|error| io_error(&from, error))?;
            if !inside(roots.source, &lexical_target)
                || !inside(roots.canonical_source, &canonical_target)
            {
                return Err(WorkspaceError::EscapingLink {
                    path: from,
                    target: link,
                });
            }
            let local_target = lexical_target
                .strip_prefix(roots.source)
                .map_err(|_| WorkspaceError::UnsafePath(lexical_target.clone()))?;
            let relocated = roots.destination.join(local_target);
            let target_metadata = fs::metadata(&canonical_target)
                .map_err(|error| io_error(&canonical_target, error))?;
            let isolated_link = if cfg!(windows) && target_metadata.is_dir() {
                roots.final_destination.join(local_target)
            } else if link.is_absolute() {
                pathdiff(&to, &relocated)?
            } else {
                link
            };
            create_link(&isolated_link, &to, target_metadata.is_dir())
                .map_err(|error| io_error(&to, error))?;
        } else if metadata.file_type().is_file() {
            operations.copy_file(&from, &to)?;
        } else {
            return Err(WorkspaceError::UnsupportedEntry(from));
        }
    }
    Ok(())
}

fn pathdiff(from: &Path, to: &Path) -> Result<PathBuf, WorkspaceError> {
    let from = from
        .parent()
        .ok_or_else(|| WorkspaceError::UnsafePath(from.into()))?;
    let from_components = from.components().collect::<Vec<_>>();
    let to_components = to.components().collect::<Vec<_>>();
    let common = from_components
        .iter()
        .zip(&to_components)
        .take_while(|(left, right)| left == right)
        .count();
    if common == 0 {
        return Err(WorkspaceError::UnsafePath(to.into()));
    }
    let mut relative = PathBuf::new();
    for _ in common..from_components.len() {
        relative.push("..");
    }
    for component in &to_components[common..] {
        relative.push(component.as_os_str());
    }
    Ok(relative)
}

fn link_node_modules<Operations: WorkspaceOperations>(
    root: &Path,
    workspace: &Path,
    operations: &mut Operations,
) -> Result<(), WorkspaceError> {
    #[cfg(unix)]
    let _ = &operations;
    let source = root.join("node_modules");
    let source_metadata = match fs::symlink_metadata(&source) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(io_error(&source, error)),
    };
    if !source_metadata.file_type().is_dir() {
        return Err(WorkspaceError::UnsafePath(source));
    }
    let destination = workspace.join("node_modules");
    fs::create_dir_all(&destination).map_err(|error| io_error(&destination, error))?;
    let mut entries = fs::read_dir(&source)
        .map_err(|error| io_error(&source, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(&source, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let target = entry.path();
        let to = destination.join(entry.file_name());
        #[cfg(unix)]
        create_link(&target, &to, false).map_err(|error| io_error(&to, error))?;
        #[cfg(windows)]
        {
            let file_type = entry
                .file_type()
                .map_err(|error| io_error(&target, error))?;
            let resolved = if file_type.is_symlink() {
                fs::canonicalize(&target).map_err(|error| io_error(&target, error))?
            } else {
                target.clone()
            };
            let metadata = fs::metadata(&resolved).map_err(|error| io_error(&resolved, error))?;
            if metadata.is_dir() {
                create_link(&resolved, &to, true).map_err(|error| io_error(&to, error))?;
            } else if metadata.is_file() {
                // Top-level node_modules metadata files are cheap to copy and a
                // link would unnecessarily require Windows symlink privileges.
                operations.copy_file(&resolved, &to)?;
            } else {
                return Err(WorkspaceError::UnsupportedEntry(target));
            }
        }
    }
    Ok(())
}

fn require_lock(root: &Path, lock: &ProjectLock) -> Result<(), WorkspaceError> {
    if lock.protects(root) {
        Ok(())
    } else {
        Err(WorkspaceError::MissingLock)
    }
}

pub fn prepare_isolated_workspace(
    root: &Path,
    run_id: &str,
    lock: &ProjectLock,
) -> Result<PathBuf, WorkspaceError> {
    require_lock(root, lock)?;
    let mut operations = SystemWorkspaceOperations;
    let workspace = isolated_workspace_path(root, run_id)?;
    remove_stored_tree_deferred(root, &workspace)?;
    let canonical_root = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    copy_tree(
        root,
        &workspace,
        CopyRoots {
            source: root,
            destination: &workspace,
            final_destination: &workspace,
            canonical_source: &canonical_root,
        },
        true,
        &mut operations,
    )?;
    link_node_modules(root, &workspace, &mut operations)?;
    Ok(workspace)
}

fn transaction_prefix(root: &Path, kind: &str) -> Result<String, WorkspaceError> {
    Ok(format!(
        ".{}.{}-",
        project_name(root)?.to_string_lossy(),
        kind
    ))
}

fn transaction_path(root: &Path, kind: &str) -> Result<PathBuf, WorkspaceError> {
    let workspace = cached_workspace_path(root)?;
    Ok(workspace.parent().expect("workspace parent").join(format!(
        "{}{}",
        transaction_prefix(root, kind)?,
        unique()
    )))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CacheRecoveryResult {
    pub restored_previous: bool,
    pub removed_staging: usize,
    pub removed_previous: usize,
}

pub fn recover_cached_workspace(
    root: &Path,
    lock: &ProjectLock,
) -> Result<CacheRecoveryResult, WorkspaceError> {
    require_lock(root, lock)?;
    let workspace = cached_workspace_path(root)?;
    let parent = workspace.parent().expect("workspace parent");
    let entries = match fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CacheRecoveryResult {
                restored_previous: false,
                removed_staging: 0,
                removed_previous: 0,
            });
        }
        Err(error) => return Err(io_error(parent, error)),
    };
    let staging_prefix = transaction_prefix(root, "staging")?;
    let previous_prefix = transaction_prefix(root, "previous")?;
    let mut staging = Vec::new();
    let mut previous = Vec::new();
    let mut invalid_previous = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| io_error(parent, error))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with(&staging_prefix) {
            staging.push(entry.path());
        } else if name.starts_with(&previous_prefix) {
            let metadata = entry
                .file_type()
                .map_err(|error| io_error(&entry.path(), error))?;
            if metadata.is_dir() {
                let modified = entry
                    .metadata()
                    .and_then(|metadata| metadata.modified())
                    .unwrap_or(UNIX_EPOCH);
                previous.push((modified, entry.path()));
            } else {
                invalid_previous.push(entry.path());
            }
        }
    }
    previous.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let mut restored = false;
    if fs::symlink_metadata(&workspace).is_err()
        && let Some((_, newest)) = previous.first()
    {
        atomic_rename(newest, &workspace)?;
        previous.remove(0);
        restored = true;
    }
    let removed_staging = staging.len();
    let removed_previous = previous.len() + invalid_previous.len();
    for path in staging
        .into_iter()
        .chain(previous.into_iter().map(|(_, path)| path))
        .chain(invalid_previous)
    {
        remove_stored_tree_deferred(root, &path)?;
    }
    Ok(CacheRecoveryResult {
        restored_previous: restored,
        removed_staging,
        removed_previous,
    })
}

fn checked_reuse_path(workspace: &Path, requested: &Path) -> Result<PathBuf, WorkspaceError> {
    if requested.is_absolute()
        || requested
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceError::UnsafePath(requested.into()));
    }
    let path = workspace.join(requested);
    if path == workspace || fs::symlink_metadata(&path).is_err() {
        return Err(WorkspaceError::UnsafePath(requested.into()));
    }
    Ok(path)
}

pub fn prepare_cached_workspace(
    root: &Path,
    lock: &ProjectLock,
    reuse_paths: &[PathBuf],
) -> Result<PathBuf, WorkspaceError> {
    let mut operations = SystemWorkspaceOperations;
    prepare_cached_workspace_with_operations(root, lock, reuse_paths, &mut operations)
}

fn cargo_transaction_prefix(root: &Path, kind: &str) -> Result<String, WorkspaceError> {
    Ok(format!(
        ".{}.{}-",
        project_name(root)?.to_string_lossy(),
        kind
    ))
}

fn cargo_transaction_path(
    root: &Path,
    container: &Path,
    kind: &str,
) -> Result<PathBuf, WorkspaceError> {
    Ok(container.join(format!(
        "{}{}",
        cargo_transaction_prefix(root, kind)?,
        unique()
    )))
}

fn validate_cargo_descendant(container: &Path, target: &Path) -> Result<(), WorkspaceError> {
    let local = target
        .strip_prefix(container)
        .map_err(|_| WorkspaceError::UnsafePath(target.into()))?;
    if local.as_os_str().is_empty()
        || local
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(WorkspaceError::UnsafePath(target.into()));
    }
    let mut current = container.to_owned();
    for component in local.components() {
        current.push(component.as_os_str());
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(WorkspaceError::UnsafePath(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(error) => return Err(io_error(&current, error)),
        }
    }
    Ok(())
}

fn remove_cargo_owned_tree(container: &Path, target: &Path) -> Result<bool, WorkspaceError> {
    validate_cargo_descendant(container, target)?;
    match fs::symlink_metadata(target) {
        Ok(metadata) if metadata.file_type().is_dir() => {
            fs::remove_dir_all(target).map_err(|error| io_error(target, error))?;
            Ok(true)
        }
        Ok(_) => Err(WorkspaceError::UnsafePath(target.into())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(target, error)),
    }
}

fn recover_cargo_workspace_at(
    root: &Path,
    container: &Path,
    workspace: &Path,
) -> Result<CacheRecoveryResult, WorkspaceError> {
    let staging_prefix = cargo_transaction_prefix(root, "staging")?;
    let previous_prefix = cargo_transaction_prefix(root, "previous")?;
    let mut staging = Vec::new();
    let mut previous = Vec::new();
    for entry in fs::read_dir(container)
        .map_err(|error| io_error(container, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(container, error))?
    {
        let name = entry.file_name().to_string_lossy().into_owned();
        let metadata = entry
            .file_type()
            .map_err(|error| io_error(&entry.path(), error))?;
        if name.starts_with(&staging_prefix) {
            if !metadata.is_dir() {
                return Err(WorkspaceError::UnsafePath(entry.path()));
            }
            staging.push(entry.path());
        } else if name.starts_with(&previous_prefix) {
            if !metadata.is_dir() {
                return Err(WorkspaceError::UnsafePath(entry.path()));
            }
            let modified = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .unwrap_or(UNIX_EPOCH);
            previous.push((modified, entry.path()));
        }
    }
    previous.sort_by_key(|entry| std::cmp::Reverse(entry.0));
    let mut restored = false;
    if fs::symlink_metadata(workspace).is_err()
        && let Some((_, newest)) = previous.first()
    {
        fs::create_dir_all(workspace.parent().expect("Cargo workspace parent"))
            .map_err(|error| io_error(workspace, error))?;
        atomic_rename(newest, workspace)?;
        previous.remove(0);
        restored = true;
    }
    let removed_staging = staging.len();
    let removed_previous = previous.len();
    for path in staging
        .into_iter()
        .chain(previous.into_iter().map(|(_, path)| path))
    {
        remove_cargo_owned_tree(container, &path)?;
    }
    Ok(CacheRecoveryResult {
        restored_previous: restored,
        removed_staging,
        removed_previous,
    })
}

pub fn recover_cargo_cached_workspace(
    root: &Path,
    lock: &ProjectLock,
) -> Result<CacheRecoveryResult, WorkspaceError> {
    require_lock(root, lock)?;
    let container = ensure_cargo_container(root)?;
    let workspace = cargo_cached_workspace_path(root)?;
    recover_cargo_workspace_at(root, &container, &workspace)
}

pub fn prepare_cargo_cached_workspace(
    root: &Path,
    lock: &ProjectLock,
) -> Result<PathBuf, WorkspaceError> {
    let mut operations = SystemWorkspaceOperations;
    prepare_cargo_cached_workspace_with_operations(root, lock, &mut operations)
}

fn prepare_cargo_cached_workspace_with_operations<Operations: WorkspaceOperations>(
    root: &Path,
    lock: &ProjectLock,
    operations: &mut Operations,
) -> Result<PathBuf, WorkspaceError> {
    require_lock(root, lock)?;
    let container = ensure_cargo_container(root)?;
    let workspace = cargo_cached_workspace_path(root)?;
    recover_cargo_workspace_at(root, &container, &workspace)?;
    let staging = cargo_transaction_path(root, &container, "staging")?;
    let previous = cargo_transaction_path(root, &container, "previous")?;
    let result = (|| {
        let canonical_root = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
        copy_tree(
            root,
            &staging,
            CopyRoots {
                source: root,
                destination: &staging,
                final_destination: &workspace,
                canonical_source: &canonical_root,
            },
            true,
            operations,
        )?;
        link_node_modules(root, &staging, operations)?;
        fs::create_dir_all(workspace.parent().expect("Cargo workspace parent"))
            .map_err(|error| io_error(&workspace, error))?;
        let mut moved_previous = false;
        if fs::symlink_metadata(&workspace).is_ok() {
            operations.rename(&workspace, &previous)?;
            moved_previous = true;
        }
        if let Err(error) = operations.rename(&staging, &workspace) {
            if moved_previous && fs::symlink_metadata(&workspace).is_err() {
                let _ = operations.rename(&previous, &workspace);
            }
            return Err(error);
        }
        if fs::symlink_metadata(&previous).is_ok() {
            remove_cargo_owned_tree(&container, &previous)?;
        }
        Ok(workspace.clone())
    })();
    if fs::symlink_metadata(&staging).is_ok() {
        remove_cargo_owned_tree(&container, &staging)?;
    }
    result
}

pub fn remove_cargo_workspace_run(root: &Path, run_id: &str) -> Result<bool, WorkspaceError> {
    let container = cargo_workspace_container(root)?;
    match fs::symlink_metadata(&container) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(&container, error)),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(WorkspaceError::UnsafePath(container));
        }
        Ok(_) => {}
    }
    if read_cargo_marker(&container.join(WORKSPACE_MARKER))? != expected_cargo_marker(root)? {
        return Err(WorkspaceError::UnsafePath(container));
    }
    let workspace = cargo_cached_workspace_path(root)?;
    remove_cargo_owned_tree(&container, &workspace.join(".supercov/work").join(run_id))
}

pub fn clean_cargo_workspace(root: &Path, dry_run: bool) -> Result<bool, WorkspaceError> {
    let container = cargo_workspace_container(root)?;
    match fs::symlink_metadata(&container) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(io_error(&container, error)),
        Ok(metadata) if !metadata.file_type().is_dir() => {
            return Err(WorkspaceError::UnsafePath(container));
        }
        Ok(_) => {}
    }
    if read_cargo_marker(&container.join(WORKSPACE_MARKER))? != expected_cargo_marker(root)? {
        return Err(WorkspaceError::UnsafePath(container));
    }
    if !dry_run {
        fs::remove_dir_all(&container).map_err(|error| io_error(&container, error))?;
    }
    Ok(true)
}

fn prepare_cached_workspace_with_operations<Operations: WorkspaceOperations>(
    root: &Path,
    lock: &ProjectLock,
    reuse_paths: &[PathBuf],
    operations: &mut Operations,
) -> Result<PathBuf, WorkspaceError> {
    require_lock(root, lock)?;
    ensure_container(root)?;
    recover_cached_workspace(root, lock)?;
    let workspace = cached_workspace_path(root)?;
    let staging = transaction_path(root, "staging")?;
    let previous = transaction_path(root, "previous")?;
    let result = (|| {
        let canonical_root = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
        copy_tree(
            root,
            &staging,
            CopyRoots {
                source: root,
                destination: &staging,
                final_destination: &workspace,
                canonical_source: &canonical_root,
            },
            true,
            operations,
        )?;
        link_node_modules(root, &staging, operations)?;
        for requested in reuse_paths {
            let from = checked_reuse_path(&workspace, requested)?;
            let to = staging.join(requested);
            let metadata = fs::symlink_metadata(&from).map_err(|error| io_error(&from, error))?;
            if metadata.file_type().is_dir() {
                let canonical_workspace =
                    fs::canonicalize(&workspace).map_err(|error| io_error(&workspace, error))?;
                copy_tree(
                    &from,
                    &to,
                    CopyRoots {
                        source: &workspace,
                        destination: &staging,
                        final_destination: &workspace,
                        canonical_source: &canonical_workspace,
                    },
                    false,
                    operations,
                )?;
            } else if metadata.file_type().is_file() {
                fs::create_dir_all(to.parent().expect("reuse parent"))
                    .map_err(|error| io_error(&to, error))?;
                operations.copy_file(&from, &to)?;
            } else {
                return Err(WorkspaceError::UnsupportedEntry(from));
            }
        }
        let mut moved_previous = false;
        if fs::symlink_metadata(&workspace).is_ok() {
            operations.rename(&workspace, &previous)?;
            moved_previous = true;
        }
        if let Err(error) = operations.rename(&staging, &workspace) {
            if moved_previous && fs::symlink_metadata(&workspace).is_err() {
                let _ = operations.rename(&previous, &workspace);
            }
            return Err(error);
        }
        if fs::symlink_metadata(&previous).is_ok() {
            remove_stored_tree_deferred(root, &previous)?;
        }
        Ok(workspace.clone())
    })();
    if fs::symlink_metadata(&staging).is_ok() {
        remove_stored_tree_deferred(root, &staging)?;
    }
    result
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct BuildCacheMetadata {
    #[serde(default)]
    artifact_paths: Vec<String>,
}

fn retain_cached_artifact_roots(
    keep: &mut BTreeSet<String>,
    artifact_paths: impl IntoIterator<Item = String>,
) {
    for artifact in artifact_paths {
        if let Some(top) = Path::new(&artifact)
            .components()
            .next()
            .and_then(|component| match component {
                Component::Normal(value) => value.to_str(),
                _ => None,
            })
        {
            keep.insert(top.into());
        }
    }
}

pub fn prune_cached_workspace_sources(
    root: &Path,
    lock: &ProjectLock,
) -> Result<Vec<String>, WorkspaceError> {
    require_lock(root, lock)?;
    let workspace = cached_workspace_path(root)?;
    if fs::symlink_metadata(&workspace).is_err() {
        return Ok(Vec::new());
    }
    let mut keep = BTreeSet::from(["node_modules".to_owned(), ".supercov".to_owned()]);
    let metadata_path = workspace.join(".supercov/build-cache.json");
    if let Ok(bytes) = fs::read(&metadata_path) {
        let metadata: BuildCacheMetadata =
            serde_json::from_slice(&bytes).map_err(WorkspaceError::InvalidCacheMetadata)?;
        retain_cached_artifact_roots(&mut keep, metadata.artifact_paths);
    }
    let mut removed = Vec::new();
    for entry in fs::read_dir(&workspace).map_err(|error| io_error(&workspace, error))? {
        let entry = entry.map_err(|error| io_error(&workspace, error))?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| WorkspaceError::UnsafePath(entry.path()))?;
        if keep.contains(&name) {
            continue;
        }
        remove_stored_tree_deferred(root, &entry.path())?;
        removed.push(name);
    }
    removed.sort();
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct OrdinaryCopyOperations;

    impl WorkspaceOperations for OrdinaryCopyOperations {
        fn copy_file(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
            fs::copy(source, destination)
                .map(|_| ())
                .map_err(|error| io_error(destination, error))
        }

        fn rename(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
            atomic_rename(source, destination).map_err(WorkspaceError::from)
        }
    }

    struct FaultOperations {
        copy_count: usize,
        fail_copy_at: Option<usize>,
        rename_count: usize,
        fail_rename_at: Option<usize>,
    }

    impl WorkspaceOperations for FaultOperations {
        fn copy_file(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
            self.copy_count += 1;
            if self.fail_copy_at == Some(self.copy_count) {
                return Err(io_error(
                    destination,
                    io::Error::new(io::ErrorKind::StorageFull, "injected disk full"),
                ));
            }
            SystemWorkspaceOperations.copy_file(source, destination)
        }

        fn rename(&mut self, source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
            self.rename_count += 1;
            if self.fail_rename_at == Some(self.rename_count) {
                return Err(io_error(
                    destination,
                    io::Error::other("injected publication rename failure"),
                ));
            }
            SystemWorkspaceOperations.rename(source, destination)
        }
    }

    fn transaction_debris(root: &Path) -> Vec<String> {
        let workspace = cached_workspace_path(root).unwrap();
        let prefix = format!(".{}.", project_name(root).unwrap().to_string_lossy());
        fs::read_dir(workspace.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| name.starts_with(&prefix))
            .collect()
    }

    fn project() -> PathBuf {
        let root = std::env::temp_dir().join(format!("supercov-workspace-rust-{}", unique()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/index.js"), "one").unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        root
    }

    #[test]
    fn isolated_copy_never_changes_source_and_requires_the_lock() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_isolated_workspace(&root, "run", &lock).unwrap();
        fs::write(workspace.join("src/index.js"), "instrumented").unwrap();
        assert_eq!(
            fs::read_to_string(root.join("src/index.js")).unwrap(),
            "one"
        );
        lock.release().unwrap();
        assert!(matches!(
            prepare_isolated_workspace(&root, "other", &lock),
            Err(WorkspaceError::MissingLock)
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cargo_cache_is_an_owned_same_parent_sibling_and_cleans_exactly() {
        let root = project();
        fs::create_dir_all(root.join(".cargo")).unwrap();
        fs::write(
            root.join(".cargo/config.toml"),
            "[build]\nrustflags=[\"--cfg\",\"copied-once\"]\n",
        )
        .unwrap();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cargo_cached_workspace(&root, &lock).unwrap();
        let container = cargo_workspace_container(&root).unwrap();
        assert_eq!(
            container.parent(),
            fs::canonicalize(&root).unwrap().parent()
        );
        assert!(!workspace.starts_with(&root));
        assert_eq!(workspace.file_name(), root.file_name());
        assert_eq!(
            fs::read_to_string(workspace.join(".cargo/config.toml")).unwrap(),
            "[build]\nrustflags=[\"--cfg\",\"copied-once\"]\n"
        );
        fs::create_dir_all(workspace.join(".supercov/work/run_1")).unwrap();
        assert!(remove_cargo_workspace_run(&root, "run_1").unwrap());
        assert!(!workspace.join(".supercov/work/run_1").exists());
        assert!(clean_cargo_workspace(&root, true).unwrap());
        assert!(container.exists());
        assert!(clean_cargo_workspace(&root, false).unwrap());
        assert!(!container.exists());
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cargo_cache_rejects_a_tampered_marker_without_deleting_it() {
        let root = project();
        let container = cargo_workspace_container(&root).unwrap();
        fs::create_dir(&container).unwrap();
        fs::write(container.join(WORKSPACE_MARKER), "{}\n").unwrap();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        assert!(matches!(
            prepare_cargo_cached_workspace(&root, &lock),
            Err(WorkspaceError::InvalidCacheMetadata(_)) | Err(WorkspaceError::UnsafePath(_))
        ));
        assert!(container.exists());
        lock.release().unwrap();
        fs::remove_dir_all(container).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cargo_cache_copy_and_rename_failures_preserve_the_complete_generation() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cargo_cached_workspace(&root, &lock).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );
        fs::write(root.join("src/index.js"), "two").unwrap();

        let mut copy_failure = FaultOperations {
            copy_count: 0,
            fail_copy_at: Some(1),
            rename_count: 0,
            fail_rename_at: None,
        };
        assert!(matches!(
            prepare_cargo_cached_workspace_with_operations(&root, &lock, &mut copy_failure),
            Err(WorkspaceError::Io { .. })
        ));
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );

        let mut rename_failure = FaultOperations {
            copy_count: 0,
            fail_copy_at: None,
            rename_count: 0,
            fail_rename_at: Some(2),
        };
        assert!(matches!(
            prepare_cargo_cached_workspace_with_operations(&root, &lock, &mut rename_failure),
            Err(WorkspaceError::Io { .. })
        ));
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );

        let container = cargo_workspace_container(&root).unwrap();
        let prefix = format!(".{}.", project_name(&root).unwrap().to_string_lossy());
        assert!(
            fs::read_dir(&container)
                .unwrap()
                .filter_map(Result::ok)
                .all(|entry| !entry.file_name().to_string_lossy().starts_with(&prefix))
        );
        clean_cargo_workspace(&root, false).unwrap();
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn stable_cache_refreshes_atomically_and_reuses_only_explicit_artifacts() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let first = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        fs::create_dir_all(first.join("build")).unwrap();
        fs::write(first.join("build/output.js"), "instrumented").unwrap();
        fs::write(first.join("stale.txt"), "stale").unwrap();
        fs::write(root.join("src/index.js"), "two").unwrap();
        let second = prepare_cached_workspace(&root, &lock, &[PathBuf::from("build")]).unwrap();
        assert_eq!(first, second);
        assert_eq!(
            fs::read_to_string(second.join("src/index.js")).unwrap(),
            "two"
        );
        assert_eq!(
            fs::read_to_string(second.join("build/output.js")).unwrap(),
            "instrumented"
        );
        assert!(!second.join("stale.txt").exists());
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn terminal_workspace_keeps_flat_frontend_cache_without_discoverable_test_sources() {
        let root = project();
        fs::create_dir_all(root.join("tests/e2e")).unwrap();
        fs::write(root.join("tests/e2e/app.spec.js"), "test source").unwrap();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        let artifacts = workspace.join(".supercov/frontend-cache-artifacts");
        fs::create_dir_all(&artifacts).unwrap();
        fs::write(artifacts.join("digest"), "instrumented test").unwrap();
        fs::write(
            workspace.join(".supercov/frontend-cache.json"),
            "{\"schemaVersion\":2}",
        )
        .unwrap();
        prune_cached_workspace_sources(&root, &lock).unwrap();
        assert!(!workspace.join("tests/e2e/app.spec.js").exists());
        assert_eq!(
            fs::read_to_string(artifacts.join("digest")).unwrap(),
            "instrumented test"
        );
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn ordinary_copy_fallback_preserves_workspace_semantics() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let mut operations = OrdinaryCopyOperations;
        let workspace =
            prepare_cached_workspace_with_operations(&root, &lock, &[], &mut operations).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );
        assert_eq!(
            fs::read_to_string(root.join("src/index.js")).unwrap(),
            "one"
        );
        assert!(transaction_debris(&root).is_empty());
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn enospc_during_copy_preserves_the_complete_generation() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        fs::write(workspace.join("generation"), "complete").unwrap();
        fs::write(root.join("src/index.js"), "new source").unwrap();
        let mut operations = FaultOperations {
            copy_count: 0,
            fail_copy_at: Some(1),
            rename_count: 0,
            fail_rename_at: None,
        };
        let error = prepare_cached_workspace_with_operations(&root, &lock, &[], &mut operations)
            .unwrap_err();
        assert!(matches!(
            error,
            WorkspaceError::Io { ref source, .. }
                if source.kind() == io::ErrorKind::StorageFull
        ));
        assert_eq!(
            fs::read_to_string(workspace.join("generation")).unwrap(),
            "complete"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );
        assert!(transaction_debris(&root).is_empty());
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_publication_rename_restores_the_complete_generation() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        fs::write(workspace.join("generation"), "complete").unwrap();
        fs::write(root.join("src/index.js"), "new source").unwrap();
        let mut operations = FaultOperations {
            copy_count: 0,
            fail_copy_at: None,
            rename_count: 0,
            fail_rename_at: Some(2),
        };
        let error = prepare_cached_workspace_with_operations(&root, &lock, &[], &mut operations)
            .unwrap_err();
        assert!(matches!(
            error,
            WorkspaceError::Io { ref source, .. }
                if source.kind() == io::ErrorKind::Other
        ));
        assert_eq!(
            operations.rename_count, 3,
            "prior generation was not restored"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("generation")).unwrap(),
            "complete"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );
        assert!(transaction_debris(&root).is_empty());
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn relocates_internal_links_and_rejects_external_links() {
        use std::os::unix::fs::symlink;

        let root = project();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(root.join("shared/value"), "inside").unwrap();
        symlink("../shared/value", root.join("src/value-link")).unwrap();
        let external = project();
        fs::write(external.join("outside"), "outside").unwrap();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("src/value-link")).unwrap(),
            "inside"
        );
        symlink(external.join("outside"), root.join("src/external-link")).unwrap();
        assert!(matches!(
            prepare_cached_workspace(&root, &lock, &[]),
            Err(WorkspaceError::EscapingLink { .. })
        ));
        assert_eq!(
            fs::read_to_string(workspace.join("src/index.js")).unwrap(),
            "one"
        );
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(external).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn relocates_internal_junctions_without_symlink_privileges() {
        let root = project();
        fs::create_dir_all(root.join("shared")).unwrap();
        fs::write(root.join("shared/value"), "inside").unwrap();
        junction::create(root.join("shared"), root.join("linked-shared")).unwrap();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        let isolated_link = workspace.join("linked-shared");
        assert!(junction::exists(&isolated_link).unwrap());
        assert_eq!(
            fs::canonicalize(junction::get_target(&isolated_link).unwrap()).unwrap(),
            fs::canonicalize(workspace.join("shared")).unwrap()
        );
        assert_eq!(
            fs::read_to_string(isolated_link.join("value")).unwrap(),
            "inside"
        );
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn mounts_node_modules_with_junctions_and_copies_metadata_files() {
        let root = project();
        fs::create_dir_all(root.join("node_modules/example")).unwrap();
        fs::write(root.join("node_modules/example/index.js"), "module").unwrap();
        fs::write(root.join("node_modules/.package-lock.json"), "lock").unwrap();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        let workspace = prepare_cached_workspace(&root, &lock, &[]).unwrap();
        let package = workspace.join("node_modules/example");
        assert!(junction::exists(&package).unwrap());
        assert_eq!(
            fs::canonicalize(junction::get_target(&package).unwrap()).unwrap(),
            fs::canonicalize(root.join("node_modules/example")).unwrap()
        );
        let metadata = workspace.join("node_modules/.package-lock.json");
        assert!(fs::symlink_metadata(&metadata).unwrap().is_file());
        assert_eq!(fs::read_to_string(metadata).unwrap(), "lock");
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovery_restores_the_newest_previous_generation_and_discards_staging() {
        let root = project();
        let mut lock = ProjectLock::acquire(&root, "run", "now").unwrap();
        ensure_container(&root).unwrap();
        let workspace = cached_workspace_path(&root).unwrap();
        let parent = workspace.parent().unwrap();
        fs::create_dir_all(parent).unwrap();
        let previous = parent.join(format!(
            "{}old",
            transaction_prefix(&root, "previous").unwrap()
        ));
        let staging = parent.join(format!(
            "{}new",
            transaction_prefix(&root, "staging").unwrap()
        ));
        fs::create_dir_all(&previous).unwrap();
        fs::write(previous.join("complete"), "yes").unwrap();
        fs::create_dir_all(&staging).unwrap();
        let result = recover_cached_workspace(&root, &lock).unwrap();
        assert!(result.restored_previous);
        assert_eq!(result.removed_staging, 1);
        assert_eq!(
            fs::read_to_string(workspace.join("complete")).unwrap(),
            "yes"
        );
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }
}
