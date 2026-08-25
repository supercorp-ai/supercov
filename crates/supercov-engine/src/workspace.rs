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

use crate::lifecycle::{LifecycleError, ProjectLock, atomic_rename, remove_stored_tree_deferred};

const WORKSPACE_MARKER: &str = ".supercov-workspace-store";
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
    root.join("supercov")
}

pub fn cached_workspace_path(root: &Path) -> Result<PathBuf, WorkspaceError> {
    Ok(workspace_container(root)
        .join("workspace")
        .join(project_name(root)?))
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
            fs::create_dir(&container).map_err(|source| io_error(&container, source))?;
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

fn copy_file(source: &Path, destination: &Path) -> Result<(), WorkspaceError> {
    reflink_copy::reflink_or_copy(source, destination)
        .map(|_| ())
        .map_err(|source_error| io_error(destination, source_error))
}

#[cfg(unix)]
fn create_link(target: &Path, destination: &Path, _directory: bool) -> io::Result<()> {
    std::os::unix::fs::symlink(target, destination)
}

#[cfg(windows)]
fn create_link(target: &Path, destination: &Path, directory: bool) -> io::Result<()> {
    if directory {
        std::os::windows::fs::symlink_dir(target, destination)
    } else {
        std::os::windows::fs::symlink_file(target, destination)
    }
}

fn copy_tree(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
    final_destination_root: &Path,
    canonical_source_root: &Path,
    root_level: bool,
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
            copy_tree(
                &from,
                &to,
                source_root,
                destination_root,
                final_destination_root,
                canonical_source_root,
                false,
            )?;
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
            if !inside(source_root, &lexical_target)
                || !inside(canonical_source_root, &canonical_target)
            {
                return Err(WorkspaceError::EscapingLink {
                    path: from,
                    target: link,
                });
            }
            let local_target = lexical_target
                .strip_prefix(source_root)
                .map_err(|_| WorkspaceError::UnsafePath(lexical_target.clone()))?;
            let relocated = destination_root.join(local_target);
            let target_metadata = fs::metadata(&canonical_target)
                .map_err(|error| io_error(&canonical_target, error))?;
            let isolated_link = if cfg!(windows) && target_metadata.is_dir() {
                final_destination_root.join(local_target)
            } else if link.is_absolute() {
                pathdiff(&to, &relocated)?
            } else {
                link
            };
            create_link(&isolated_link, &to, target_metadata.is_dir())
                .map_err(|error| io_error(&to, error))?;
        } else if metadata.file_type().is_file() {
            copy_file(&from, &to)?;
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

fn link_node_modules(root: &Path, workspace: &Path) -> Result<(), WorkspaceError> {
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
        let directory = entry
            .file_type()
            .map_err(|error| io_error(&target, error))?
            .is_dir();
        create_link(&target, &to, directory).map_err(|error| io_error(&to, error))?;
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
    let workspace = isolated_workspace_path(root, run_id)?;
    remove_stored_tree_deferred(root, &workspace)?;
    let canonical_root = fs::canonicalize(root).map_err(|error| io_error(root, error))?;
    copy_tree(
        root,
        &workspace,
        root,
        &workspace,
        &workspace,
        &canonical_root,
        true,
    )?;
    link_node_modules(root, &workspace)?;
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
    previous.sort_by(|left, right| right.0.cmp(&left.0));
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
            root,
            &staging,
            &workspace,
            &canonical_root,
            true,
        )?;
        link_node_modules(root, &staging)?;
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
                    &workspace,
                    &staging,
                    &workspace,
                    &canonical_workspace,
                    false,
                )?;
            } else if metadata.file_type().is_file() {
                fs::create_dir_all(to.parent().expect("reuse parent"))
                    .map_err(|error| io_error(&to, error))?;
                copy_file(&from, &to)?;
            } else {
                return Err(WorkspaceError::UnsupportedEntry(from));
            }
        }
        let mut moved_previous = false;
        if fs::symlink_metadata(&workspace).is_ok() {
            atomic_rename(&workspace, &previous)?;
            moved_previous = true;
        }
        if let Err(error) = atomic_rename(&staging, &workspace) {
            if moved_previous && fs::symlink_metadata(&workspace).is_err() {
                let _ = atomic_rename(&previous, &workspace);
            }
            return Err(error.into());
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
        for artifact in metadata.artifact_paths {
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
