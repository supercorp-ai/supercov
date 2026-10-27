//! Crash-safe run publication, recovery and explicit retention.
//!
//! Deletion targets are derived from a trusted project root. Large trees are
//! atomically moved into durable trash; recursive unlinking is a separate,
//! retryable operation that the CLI can run in a detached child.

use crate::fault_fs::{self as fs, File, OpenOptions};
use std::{
    collections::BTreeSet,
    io::{self, Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::run_store::{RunMetadata, valid_run_id};

const TRASH: &str = ".supercov/.trash";
const INCOMPLETE_LOCK_GRACE: Duration = Duration::from_secs(30);
static UNIQUE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum LifecycleError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    InvalidRunId(String),
    UnsafePath(PathBuf),
    InvalidState(String),
    ActiveRun {
        run_id: String,
        pid: u32,
    },
    /// A run holds its share of the store but has not yet written its name.
    StartingRun,
    Cleaning,
    LockUnavailable,
    PublicationExists(String),
    Metadata(serde_json::Error),
    EvidenceLength {
        expected: u64,
        actual: u64,
    },
    EvidenceChanged,
}

impl std::fmt::Display for LifecycleError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::InvalidRunId(id) => write!(formatter, "invalid coverage run ID: {id}"),
            Self::UnsafePath(path) => {
                write!(
                    formatter,
                    "unsafe Supercov storage path: {}",
                    path.display()
                )
            }
            Self::InvalidState(reason) => write!(formatter, "invalid run state: {reason}"),
            // Only cleaning asks to be alone; a run takes a copy of its own.
            Self::ActiveRun { run_id, pid } => write!(
                formatter,
                "coverage run {run_id} is active in this project (pid {pid}). Its storage can be cleaned when no run is active."
            ),
            Self::StartingRun => write!(
                formatter,
                "a coverage run is active in this project, starting. Its storage can be cleaned when no run is active."
            ),
            Self::Cleaning => {
                write!(
                    formatter,
                    "this project's Supercov storage is being cleaned. No test ran: start the run again when the cleaning has finished."
                )
            }
            Self::LockUnavailable => {
                write!(formatter, "could not acquire the Supercov project lock")
            }
            Self::PublicationExists(id) => write!(formatter, "coverage run already exists: {id}"),
            Self::Metadata(error) => write!(formatter, "invalid run metadata: {error}"),
            Self::EvidenceLength { expected, actual } => write!(
                formatter,
                "evidence length changed before publication: expected {expected}, got {actual}"
            ),
            Self::EvidenceChanged => write!(formatter, "evidence changed during publication"),
        }
    }
}

impl std::error::Error for LifecycleError {}

fn io_error(path: &Path, source: io::Error) -> LifecycleError {
    LifecycleError::Io {
        path: path.to_owned(),
        source,
    }
}

fn checked_id(id: &str) -> Result<(), LifecycleError> {
    valid_run_id(id)
        .then_some(())
        .ok_or_else(|| LifecycleError::InvalidRunId(id.into()))
}

fn absolute_root(root: &Path) -> Result<PathBuf, LifecycleError> {
    if root.is_absolute() {
        Ok(root.to_owned())
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(root))
            .map_err(|source| io_error(root, source))
    }
}

fn lexical_descendant(root: &Path, path: &Path) -> bool {
    let Ok(local) = path.strip_prefix(root) else {
        return false;
    };
    !local.as_os_str().is_empty()
        && local
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
}

fn reject_linked_ancestors(
    root: &Path,
    path: &Path,
    include_leaf: bool,
) -> Result<(), LifecycleError> {
    let local = path
        .strip_prefix(root)
        .map_err(|_| LifecycleError::UnsafePath(path.into()))?;
    let components = local.components().collect::<Vec<_>>();
    let through = if include_leaf {
        components.len()
    } else {
        components.len().saturating_sub(1)
    };
    let mut current = root.to_owned();
    for component in components.into_iter().take(through) {
        let Component::Normal(component) = component else {
            return Err(LifecycleError::UnsafePath(path.into()));
        };
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(LifecycleError::UnsafePath(current));
            }
            Ok(metadata) if !metadata.file_type().is_dir() => {
                return Err(LifecycleError::UnsafePath(current));
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => break,
            Err(source) => return Err(io_error(&current, source)),
        }
    }
    Ok(())
}

fn owned_workspace_container(root: &Path) -> bool {
    let container = crate::workspace::workspace_container(root);
    crate::workspace::owned_workspace_path(&container)
}

fn unique_name() -> String {
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

/// Make a rename or creation inside `path` durable. On Unix a directory opens
/// like a file and fsync flushes its entries. On Windows there is nothing to
/// do and no way to do it: CreateFileW refuses to open a directory without
/// FILE_FLAG_BACKUP_SEMANTICS and answers ERROR_ACCESS_DENIED, which is how
/// the first Windows run died -- in the JavaScript frontend's own copy of this
/// idiom, on the generated node_modules directory it had just created -- and
/// NTFS journals directory metadata without an explicit sync. Every place
/// that syncs a directory goes through here so the rule is stated once.
pub(crate) fn sync_directory_handle(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::open(path).and_then(|directory| directory.sync_all())
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

pub(crate) fn sync_directory(path: &Path) -> Result<(), LifecycleError> {
    sync_directory_handle(path).map_err(|source| io_error(path, source))
}

pub(crate) fn atomic_rename(source: &Path, destination: &Path) -> Result<(), LifecycleError> {
    let source_parent = source
        .parent()
        .ok_or_else(|| LifecycleError::UnsafePath(source.into()))?;
    let destination_parent = destination
        .parent()
        .ok_or_else(|| LifecycleError::UnsafePath(destination.into()))?;
    fs::create_dir_all(destination_parent).map_err(|error| io_error(destination_parent, error))?;
    fs::rename(source, destination).map_err(|error| io_error(destination, error))?;
    sync_directory(destination_parent)?;
    if source_parent != destination_parent {
        sync_directory(source_parent)?;
    }
    Ok(())
}

pub(crate) fn atomic_write(root: &Path, path: &Path, bytes: &[u8]) -> Result<(), LifecycleError> {
    let parent = path
        .parent()
        .ok_or_else(|| LifecycleError::UnsafePath(path.into()))?;
    reject_linked_ancestors(root, parent, true)?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    for _ in 0..16 {
        let temporary = parent.join(format!(
            ".{}.{}.tmp",
            path.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("state"),
            unique_name()
        ));
        let mut file =
            match crate::fault::io("atomic-write-create", io::ErrorKind::AlreadyExists, || {
                fs::open_with(OpenOptions::new().write(true).create_new(true), &temporary)
            }) {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(source) => return Err(io_error(&temporary, source)),
            };
        if let Err(source) = crate::fault::io("atomic-write-sync", io::ErrorKind::Other, || {
            file.write_all(bytes).and_then(|_| file.sync_all())
        }) {
            let _ = fs::remove_file(&temporary);
            return Err(io_error(&temporary, source));
        }
        drop(file);
        if let Err(source) = fs::rename(&temporary, path) {
            let _ = fs::remove_file(&temporary);
            return Err(io_error(path, source));
        }
        sync_directory(parent)?;
        return Ok(());
    }
    Err(LifecycleError::LockUnavailable)
}

pub fn remove_stored_tree_deferred(
    project_root: &Path,
    target: &Path,
) -> Result<Option<PathBuf>, LifecycleError> {
    let root = absolute_root(project_root)?;
    let target = if target.is_absolute() {
        target.to_owned()
    } else {
        root.join(target)
    };
    match fs::symlink_metadata(&target) {
        Ok(_) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(&target, source)),
    }
    let store = root.join(".supercov");
    let trash = root.join(TRASH);
    // The workspace container is a directory of the store, so its copies are
    // stored trees like any other.
    if !lexical_descendant(&store, &target) || lexical_descendant(&trash, &target) {
        return Err(LifecycleError::UnsafePath(target));
    }
    reject_linked_ancestors(&store, &target, false)?;
    reject_linked_ancestors(&root, &trash, true)?;
    fs::create_dir_all(&trash).map_err(|source| io_error(&trash, source))?;
    let destination = trash.join(unique_name());
    atomic_rename(&target, &destination)?;
    Ok(Some(destination))
}

static BACKGROUND_SWEEP: std::sync::Mutex<Option<std::thread::JoinHandle<()>>> =
    std::sync::Mutex::new(None);

/// Empty the trash while the command runs.
///
/// A run moves the workspace it replaces into the trash before the command
/// starts, and nothing unlinked it until the command had ended: Supercov then
/// started a sweeper, waited a tenth of a second and exited, and the tree went
/// on being deleted for most of a second after. A command started in that
/// time that walks the project met a directory on its way out. AVA looks for
/// test files in every directory, `.supercov` included, and failed on
/// `EINVAL: invalid argument, scandir '.supercov/.trash/…'`.
///
/// The tree is not needed from the moment it is in the trash, so it goes while
/// the tests run.
pub fn sweep_trash_in_background(project_root: &Path) {
    let root = project_root.to_owned();
    let sweep = std::thread::spawn(move || {
        let _ = sweep_trash(&root);
    });
    // Never poisoned: nothing that holds it can panic.
    *BACKGROUND_SWEEP
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(sweep);
}

/// Wait for the sweep the run started, for as long as `patience`. True when
/// there is none left running.
pub fn finish_background_sweep(patience: Duration) -> bool {
    let Some(sweep) = BACKGROUND_SWEEP
        .lock()
        .ok()
        .and_then(|mut running| running.take())
    else {
        return true;
    };
    let deadline = std::time::Instant::now() + patience;
    while !sweep.is_finished() {
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let _ = sweep.join();
    true
}

/// Retryable trash unlinking. Public commands execute this in a child.
pub fn sweep_trash(project_root: &Path) -> Result<usize, LifecycleError> {
    let root = absolute_root(project_root)?;
    let trash = root.join(TRASH);
    reject_linked_ancestors(&root, &trash, true)?;
    let entries = match crate::fault::io("trash-list", io::ErrorKind::PermissionDenied, || {
        fs::read_dir(&trash)
    }) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(0),
        Err(source) => return Err(io_error(&trash, source)),
    };
    let Some(_lock) = TrashLock::acquire(&trash)? else {
        return Ok(0);
    };
    let mut removed = 0;
    for entry in entries {
        let entry = entry.map_err(|source| io_error(&trash, source))?;
        let path = entry.path();
        if entry.file_name() == ".deleter.lock" {
            continue;
        }
        // An entry already gone -- removed by hand, or by a sweeper whose
        // lock was replaced -- is as swept as one removed here.
        let gone = |error: &io::Error| error.kind() == io::ErrorKind::NotFound;
        let metadata = match crate::fault::io("trash-entry", io::ErrorKind::NotFound, || {
            fs::symlink_metadata(&path)
        }) {
            Ok(metadata) => metadata,
            Err(error) if gone(&error) => continue,
            Err(source) => return Err(io_error(&path, source)),
        };
        let removal = crate::fault::io("trash-remove", io::ErrorKind::NotFound, || {
            if metadata.file_type().is_dir() {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_file(&path)
            }
        });
        match removal {
            Ok(()) => removed += 1,
            Err(error) if gone(&error) => {}
            Err(source) => return Err(io_error(&path, source)),
        }
    }
    Ok(removed)
}

struct TrashLock {
    path: PathBuf,
}

impl TrashLock {
    fn acquire(trash: &Path) -> Result<Option<Self>, LifecycleError> {
        let path = trash.join(".deleter.lock");
        for _ in 0..2 {
            match crate::fault::io("trash-lock", io::ErrorKind::PermissionDenied, || {
                fs::open_with(OpenOptions::new().write(true).create_new(true), &path)
            }) {
                Ok(mut file) => {
                    write!(file, "{}", std::process::id())
                        .and_then(|_| file.sync_all())
                        .map_err(|source| io_error(&path, source))?;
                    return Ok(Some(Self { path }));
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                    let owner = fs::read_to_string(&path)
                        .ok()
                        .and_then(|value| value.parse::<u32>().ok());
                    if owner.is_some_and(process_exists) {
                        return Ok(None);
                    }
                    if owner.is_none() {
                        let age = fs::metadata(&path)
                            .and_then(|metadata| metadata.modified())
                            .ok()
                            .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                            .unwrap_or_default();
                        if age < INCOMPLETE_LOCK_GRACE {
                            return Ok(None);
                        }
                    }
                    fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
                }
                Err(source) => return Err(io_error(&path, source)),
            }
        }
        Ok(None)
    }
}

impl Drop for TrashLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunStateStatus {
    Preparing,
    Building,
    Testing,
    Publishing,
    Complete,
    Failed,
    Interrupted,
    Abandoned,
}

impl RunStateStatus {
    pub fn terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Failed | Self::Interrupted | Self::Abandoned
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RunState {
    pub id: String,
    pub pid: u32,
    pub root: String,
    pub workspace: String,
    pub started_at: String,
    pub updated_at: String,
    pub status: RunStateStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub signal: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn state_path(root: &Path, id: &str) -> PathBuf {
    root.join(".supercov/work").join(id).join("state.json")
}

pub fn write_run_state(root: &Path, state: &RunState) -> Result<(), LifecycleError> {
    checked_id(&state.id)?;
    let mut bytes = serde_json::to_vec_pretty(state).expect("plain data serializes");
    bytes.push(b'\n');
    atomic_write(root, &state_path(root, &state.id), &bytes)
}

fn read_state(root: &Path, id: &str) -> Result<Option<RunState>, LifecycleError> {
    checked_id(id)?;
    let path = state_path(root, id);
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => return Err(io_error(&path, source)),
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| LifecycleError::InvalidState(error.to_string()))
}

pub fn update_run_state(
    root: &Path,
    id: &str,
    status: RunStateStatus,
    updated_at: &str,
    error: Option<String>,
) -> Result<RunState, LifecycleError> {
    let mut state = read_state(root, id)?
        .ok_or_else(|| LifecycleError::InvalidState(format!("state is missing for {id}")))?;
    state.status = status;
    state.updated_at = updated_at.into();
    state.error = error;
    write_run_state(root, &state)?;
    Ok(state)
}

pub fn interrupt_run_state(
    root: &Path,
    id: &str,
    updated_at: &str,
    signal: &str,
) -> Result<RunState, LifecycleError> {
    let mut state = read_state(root, id)?
        .ok_or_else(|| LifecycleError::InvalidState(format!("state is missing for {id}")))?;
    state.status = RunStateStatus::Interrupted;
    state.updated_at = updated_at.into();
    state.signal = Some(signal.into());
    state.error = Some(format!("Interrupted by {signal}"));
    write_run_state(root, &state)?;
    Ok(state)
}

#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    if pid == 0 || pid > libc::pid_t::MAX as u32 {
        return false;
    }
    // SAFETY: signal zero only checks existence/permission.
    let result = unsafe { libc::kill(pid as libc::pid_t, 0) };
    result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{
        CloseHandle, ERROR_ACCESS_DENIED, GetLastError, STILL_ACTIVE,
    };
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    if pid == 0 {
        return false;
    }
    // SAFETY: a query-only handle, closed below; nothing is read through it
    // but the exit code.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        // A process another account owns refuses the query, and exists.
        return unsafe { GetLastError() } == ERROR_ACCESS_DENIED;
    }
    let mut code = 0u32;
    // SAFETY: `handle` is open and `code` outlives the call.
    let read = unsafe { GetExitCodeProcess(handle, &mut code) };
    // SAFETY: opened above and not used after this.
    unsafe { CloseHandle(handle) };
    read != 0 && code == STILL_ACTIVE as u32
}

#[cfg(not(any(unix, windows)))]
fn process_exists(pid: u32) -> bool {
    pid == std::process::id()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct LockOwner {
    run_id: String,
    pid: u32,
    started_at: String,
}

/// The most copies of a project kept apart at once. A run past it has
/// nothing to take, which says more about what started it than about the
/// number.
const MOST_COPIES: usize = 64;

/// How long a copy other than the first is kept after its last run. The
/// first is the one a run alone always takes; the others exist because runs
/// overlapped, and hold a copy of the source and whatever was built in it.
const IDLE_COPY_KEPT: Duration = Duration::from_secs(24 * 60 * 60);

/// A run's hold on a project: its share of the store, and a copy of the
/// project that is its alone for as long as it runs.
///
/// Each is a lock the operating system keeps. It gives one to one process
/// and takes it back when that process ends, however it ends, so no run has
/// to decide whether a lock it finds was left by one that died: twenty runs
/// started together each try the first copy, and nineteen go on to the next.
/// Beside each copy's lock is a file naming who holds it, for messages and
/// for the releases that wrote that file and locked nothing.
pub struct ProjectLock {
    root: PathBuf,
    owner: LockOwner,
    copy: usize,
    /// Dropped to release: the store, then the copy.
    held: Vec<Held>,
    released: bool,
}

/// The file name of a copy's lock, without its extension: `active` for the
/// first, as it was when there was one, and `active-2` for the second.
fn copy_lock_name(copy: usize) -> String {
    if copy == 1 {
        "active".into()
    } else {
        format!("active-{copy}")
    }
}

fn read_owner(path: &Path) -> Option<LockOwner> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

/// A lock that is given back when this goes, by saying so.
///
/// Closing the file would give it back too, once every copy of the file is
/// closed, and a process started at that moment holds a copy until it has
/// loaded its program. A run that released its copy and one that asked for
/// it in the same instant found it still held.
struct Held(File);

impl Drop for Held {
    fn drop(&mut self) {
        let _ = self.0.unlock();
    }
}

/// A file's lock, taken if no other process holds it. `None` is held.
///
/// Where the file system keeps no locks, `alive` decides: the owner written
/// beside the lock, if its process is still there.
fn take_lock(
    path: &Path,
    shared: bool,
    alive: impl Fn() -> bool,
) -> Result<Option<Held>, LifecycleError> {
    let file = fs::open_with(
        OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false),
        path,
    )
    .map_err(|source| io_error(path, source))?;
    let taken = crate::fault::or(
        "lock-unsupported",
        || Err(fs::TryLockError::Error(io::ErrorKind::Unsupported.into())),
        || {
            if shared {
                file.try_lock_shared()
            } else {
                file.try_lock()
            }
        },
    );
    match taken {
        Ok(()) => Ok(Some(Held(file))),
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(_)) => Ok((!alive()).then_some(Held(file))),
    }
}

/// The owner of a copy whose process is still there and is not this one.
fn live_owner(locks: &Path, copy: usize) -> Option<LockOwner> {
    read_owner(&locks.join(format!("{}.json", copy_lock_name(copy))))
        .filter(|owner| owner.pid != std::process::id() && process_exists(owner.pid))
}

impl ProjectLock {
    /// A run's hold: a share of the store, and the first copy nobody has.
    pub fn acquire(root: &Path, run_id: &str, started_at: &str) -> Result<Self, LifecycleError> {
        Self::take(root, run_id, started_at, false)
    }

    /// The whole store, for cleaning. Refused while any run is active.
    pub fn acquire_alone(
        root: &Path,
        run_id: &str,
        started_at: &str,
    ) -> Result<Self, LifecycleError> {
        Self::take(root, run_id, started_at, true)
    }

    fn take(
        root: &Path,
        run_id: &str,
        started_at: &str,
        alone: bool,
    ) -> Result<Self, LifecycleError> {
        checked_id(run_id)?;
        // A project is a directory with a name: its copies are named after it.
        if root.file_name().is_none() {
            return Err(LifecycleError::UnsafePath(root.into()));
        }
        let locks = root.join(".supercov/locks");
        reject_linked_ancestors(root, &locks, true)?;
        fs::create_dir_all(&locks).map_err(|source| io_error(&locks, source))?;
        // Direct frontends do not create an isolated workspace. Initialize
        // the store's ignore file here as well, without replacing user rules
        // or following a pre-existing symlink.
        let ignore = root.join(".supercov/.gitignore");
        match crate::fault::io("store-ignore", io::ErrorKind::PermissionDenied, || {
            fs::open_with(OpenOptions::new().write(true).create_new(true), &ignore)
        }) {
            Ok(mut file) => file
                .write_all(b"*\n")
                .map_err(|source| io_error(&ignore, source))?,
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(source) => return Err(io_error(&ignore, source)),
        }
        let owner = LockOwner {
            run_id: run_id.into(),
            pid: std::process::id(),
            started_at: started_at.into(),
        };
        let active = || (1..=MOST_COPIES).find_map(|copy| live_owner(&locks, copy));
        let store = take_lock(&locks.join("store.lock"), !alone, || {
            alone && active().is_some()
        })?;
        let Some(store) = store else {
            // Whoever holds a share of the store wrote its name beside its
            // copy; this process may be one of them.
            let holder = || {
                (1..=MOST_COPIES).find_map(|copy| {
                    read_owner(&locks.join(format!("{}.json", copy_lock_name(copy))))
                        .filter(|owner| process_exists(owner.pid))
                })
            };
            return Err(if alone {
                // A run that took its share a moment ago may not have
                // written its name yet: it is still a run, not a lock fault.
                holder().map_or(LifecycleError::StartingRun, |owner| {
                    LifecycleError::ActiveRun {
                        run_id: owner.run_id,
                        pid: owner.pid,
                    }
                })
            } else {
                LifecycleError::Cleaning
            });
        };
        // A run of a release that locked nothing shares no store lock, and
        // is known by the file it wrote.
        if alone && let Some(owner) = active() {
            return Err(LifecycleError::ActiveRun {
                run_id: owner.run_id,
                pid: owner.pid,
            });
        }
        for copy in 1..=MOST_COPIES {
            let name = copy_lock_name(copy);
            let Some(lock) = take_lock(&locks.join(format!("{name}.lock")), false, || {
                live_owner(&locks, copy).is_some()
            })?
            else {
                continue;
            };
            if live_owner(&locks, copy).is_some() {
                continue;
            }
            let path = locks.join(format!("{name}.json"));
            let mut payload = serde_json::to_vec_pretty(&owner).expect("plain data serializes");
            payload.push(b'\n');
            fs::write(&path, payload).map_err(|source| io_error(&path, source))?;
            return Ok(Self {
                root: root.to_owned(),
                owner,
                copy,
                held: vec![store, lock],
                released: false,
            });
        }
        Err(LifecycleError::LockUnavailable)
    }

    /// Which copy of the project this run has: 1 when it runs alone.
    pub fn copy(&self) -> usize {
        self.copy
    }

    pub fn release(&mut self) -> Result<(), LifecycleError> {
        if self.released {
            return Ok(());
        }
        self.released = true;
        let path = self
            .root
            .join(".supercov/locks")
            .join(format!("{}.json", copy_lock_name(self.copy)));
        if read_owner(&path).is_some_and(|owner| owner == self.owner) {
            fs::remove_file(&path).map_err(|source| io_error(&path, source))?;
        }
        // The copy was last used now, which is what keeps it a day.
        let _ = self
            .held
            .last()
            .map(|lock| lock.0.set_modified(SystemTime::now()));
        self.held.clear();
        Ok(())
    }

    pub(crate) fn protects(&self, root: &Path) -> bool {
        !self.released && self.root == root
    }
}

impl Drop for ProjectLock {
    fn drop(&mut self) {
        let _ = self.release();
    }
}

fn copy_regular_file(source: &Path, destination: &Path) -> Result<u64, LifecycleError> {
    let metadata = fs::symlink_metadata(source).map_err(|error| io_error(source, error))?;
    if !metadata.file_type().is_file() {
        return Err(LifecycleError::UnsafePath(source.into()));
    }
    // A clone where the filesystem can make one (APFS, Btrfs, XFS): the
    // archive's blocks are shared, not copied -- 150 MB on a large run --
    // and the clone is the source as it was at that instant.
    let cloned = crate::fault::or(
        "clone",
        // A test stands a copy in for the clone where the file system makes
        // none, so what follows one is checked there too.
        || fs::copy(source, destination).is_ok(),
        || reflink_copy::reflink(source, destination).is_ok(),
    );
    if cloned {
        fs::open(destination)
            .and_then(|file| file.sync_all())
            .map_err(|error| io_error(destination, error))?;
        return fs::symlink_metadata(destination)
            .map(|metadata| metadata.len())
            .map_err(|error| io_error(destination, error));
    }
    let mut input = fs::open(source).map_err(|error| io_error(source, error))?;
    let mut output = fs::open_with(OpenOptions::new().write(true).create_new(true), destination)
        .map_err(|error| io_error(destination, error))?;
    let copied = io::copy(&mut input, &mut output).map_err(|error| io_error(destination, error))?;
    output
        .sync_all()
        .map_err(|error| io_error(destination, error))?;
    Ok(copied)
}

fn file_sha256(path: &Path) -> Result<[u8; 32], LifecycleError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| io_error(path, source))?;
    if !metadata.file_type().is_file() {
        return Err(LifecycleError::UnsafePath(path.into()));
    }
    let mut file = fs::open(path).map_err(|source| io_error(path, source))?;
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|source| io_error(path, source))?;
        if read == 0 {
            break;
        }
        hash.update(&buffer[..read]);
    }
    Ok(hash.finalize().into())
}

fn hex_digest(digest: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    digest
        .iter()
        .fold(String::with_capacity(64), |mut text, byte| {
            let _ = write!(text, "{byte:02x}");
            text
        })
}

/// Publish immutable run evidence with its source manifest cache and per-test
/// execution record together.
pub fn publish_run(
    root: &Path,
    metadata: &RunMetadata,
    evidence_source: &Path,
) -> Result<PathBuf, LifecycleError> {
    publish_run_with_fault(root, metadata, evidence_source, Archived::default(), None)
}

/// What a frontend that still holds the run it archived hands publication,
/// so it need not read the archive back: the analysis of the archived
/// request, and the assertion inputs archived beside it. Each must be what
/// the archive gives; a frontend that cannot say so leaves it out.
#[derive(Default)]
pub struct Archived {
    pub report: Option<crate::coverage_report::CoverageReport>,
    pub inputs: Option<crate::source_manifest::InputManifest>,
}

/// `publish_run` for a frontend that holds what it archived; see `Archived`.
pub fn publish_archived_run(
    root: &Path,
    metadata: &RunMetadata,
    evidence_source: &Path,
    archived: Archived,
) -> Result<PathBuf, LifecycleError> {
    publish_run_with_fault(root, metadata, evidence_source, archived, None)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunPublicationFault {
    FinalRename,
    /// The files publication derives from the evidence -- the query index,
    /// the statement spans and the assessment input -- cannot be written:
    /// they are written below the staged evidence file, where nothing can be.
    DerivedWrites,
}

pub(crate) fn publish_run_with_fault(
    root: &Path,
    metadata: &RunMetadata,
    evidence_source: &Path,
    archived: Archived,
    fault: Option<RunPublicationFault>,
) -> Result<PathBuf, LifecycleError> {
    // Every run says which release recorded it.
    let mut metadata = metadata.clone();
    metadata
        .recorded_by
        .get_or_insert_with(|| env!("CARGO_PKG_VERSION").to_owned());
    let metadata = &metadata;
    checked_id(&metadata.id)?;
    let destination = root.join(".supercov/runs").join(&metadata.id);
    reject_linked_ancestors(root, &destination, false)?;
    if fs::symlink_metadata(&destination).is_ok() {
        return Err(LifecycleError::PublicationExists(metadata.id.clone()));
    }
    let staging = root
        .join(".supercov/work")
        .join(&metadata.id)
        .join("run-publication");
    reject_linked_ancestors(root, &staging, true)?;
    if fs::symlink_metadata(&staging).is_ok() {
        remove_stored_tree_deferred(root, &staging)?;
    }
    let evidence_sha256 = file_sha256(evidence_source)?;
    fs::create_dir_all(&staging).map_err(|source| io_error(&staging, source))?;
    let copied = copy_regular_file(evidence_source, &staging.join("evidence.raw.gz"))?;
    if copied != metadata.raw_evidence.compressed_bytes {
        remove_stored_tree_deferred(root, &staging)?;
        return Err(LifecycleError::EvidenceLength {
            expected: metadata.raw_evidence.compressed_bytes,
            actual: copied,
        });
    }
    // What is published is the copy: it must be the evidence as hashed
    // before it was taken.
    if crate::fault::or(
        "published-copy-differs",
        || Ok([0; 32]),
        || file_sha256(&staging.join("evidence.raw.gz")),
    )? != evidence_sha256
    {
        remove_stored_tree_deferred(root, &staging)?;
        return Err(LifecycleError::EvidenceChanged);
    }
    let mut json = serde_json::to_vec_pretty(metadata).expect("plain data serializes");
    json.push(b'\n');
    atomic_write(root, &staging.join("run.json"), &json)?;
    // Everything publication derives from the evidence -- the record of what
    // each test ran and the query index -- comes from one analysis of it, made
    // here. Each used to analyse
    // the archive on its own: once here, twice more in the first query after
    // the run, and on a 3,900-test Python suite each analysis took 5 to 8
    // seconds. Evidence that will not analyse is published as before; the
    // first query then reports why.
    let staged = crate::run_store::StoredRun {
        id: metadata.id.clone(),
        directory: staging.clone(),
        evidence_path: staging.join("evidence.raw.gz"),
        metadata_path: staging.join("run.json"),
        query_index_path: staging.join(crate::run_store::RUST_QUERY_INDEX_FILE),
        metadata: metadata.clone(),
    };
    let analysed = if metadata.merged == Some(true) {
        None
    } else if archived.report.is_some() {
        archived.report
    } else {
        crate::run_store::analyze_stored_run(&staged).ok()
    };
    let archived_inputs = match archived
        .inputs
        .map(|manifest| {
            crate::source_manifest::archived_manifest(manifest, hex_digest(&evidence_sha256))
        })
        .transpose()
    {
        Ok(inputs) => inputs,
        Err(reason) => {
            let _ = remove_stored_tree_deferred(root, &staging);
            return Err(LifecycleError::InvalidState(format!(
                "source manifest publication: {reason}"
            )));
        }
    };
    let derived = if fault == Some(RunPublicationFault::DerivedWrites) {
        let directory = staged.evidence_path.join("derived");
        crate::run_store::StoredRun {
            query_index_path: directory.join(crate::run_store::RUST_QUERY_INDEX_FILE),
            directory,
            ..staged.clone()
        }
    } else {
        staged.clone()
    };
    // The execution record and the query index both read the analysis and
    // write their own files, so they are written side by side.
    let prepared = std::thread::scope(|scope| {
        let index = analysed.as_ref().map(|report| {
            scope.spawn(|| {
                // Disposable, like every query index: one that failed to
                // write is rebuilt by the first query.
                if crate::run_store::write_query_index_from(&derived, report).is_err() {
                    let _ = fs::remove_file(&derived.query_index_path);
                }
                // Also disposable: the report analyses the evidence when the
                // file is missing.
                if crate::run_store::write_statement_spans(&derived, report).is_err() {
                    let _ = fs::remove_file(
                        derived
                            .directory
                            .join(crate::run_store::STATEMENT_SPANS_FILE),
                    );
                }
                if crate::run_store::write_assessment_input(&derived, report).is_err() {
                    let _ = fs::remove_file(
                        derived
                            .directory
                            .join(crate::run_store::ASSESSMENT_INPUT_FILE),
                    );
                }
            })
        });
        let prepared = crate::source_manifest::prepare_publication_with(
            root,
            &staging,
            metadata,
            analysed.as_ref(),
            archived_inputs,
        );
        if let Some(index) = index {
            index.join().unwrap_or_else(crate::threads::rethrow);
        }
        prepared
    });
    // Everything publication derives from the analysis is written. It is
    // gigabytes of small allocations on a large run, and freeing them is
    // nobody's wait.
    if let Some(report) = analysed {
        std::thread::spawn(move || drop(report));
    }
    if let Err(reason) = prepared {
        let _ = remove_stored_tree_deferred(root, &staging);
        return Err(LifecycleError::InvalidState(format!(
            "source manifest publication: {reason}"
        )));
    }
    sync_directory(&staging)?;
    let runs = destination.parent().expect("runs parent");
    fs::create_dir_all(runs).map_err(|source| io_error(runs, source))?;
    if fault == Some(RunPublicationFault::FinalRename) {
        let error = io_error(
            &destination,
            io::Error::new(
                io::ErrorKind::PermissionDenied,
                "injected final run publication rename failure",
            ),
        );
        let _ = remove_stored_tree_deferred(root, &staging);
        return Err(error);
    }
    fs::rename(&staging, &destination).map_err(|source| io_error(&destination, source))?;
    sync_directory(runs)?;
    Ok(destination)
}

/// Read a published run's start timestamp for retention ordering. Runs whose
/// metadata is missing or unreadable sort as oldest, matching the run inventory,
/// which leaves such directories out of its listing altogether.
fn published_started_at(root: &Path, id: &str) -> String {
    let directory = root.join(".supercov/runs").join(id);
    fs::read(directory.join("run.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .as_ref()
        .and_then(|value| value.get("startedAt"))
        .and_then(|value| value.as_str())
        .unwrap_or_default()
        .to_owned()
}

fn published_run(root: &Path, id: &str) -> bool {
    let directory = root.join(".supercov/runs").join(id);
    let metadata = fs::read(directory.join("run.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok());
    metadata
        .as_ref()
        .and_then(|value| value.get("id"))
        .and_then(|value| value.as_str())
        == Some(id)
        && fs::symlink_metadata(directory.join("evidence.raw.gz"))
            .is_ok_and(|metadata| metadata.file_type().is_file())
}

pub fn finalize_published_run(root: &Path, id: &str) -> Result<bool, LifecycleError> {
    checked_id(id)?;
    if !published_run(root, id) {
        return Ok(false);
    }
    remove_stored_tree_deferred(root, &root.join(".supercov/evidence").join(id))?;
    remove_stored_tree_deferred(root, &root.join(".supercov/work").join(id))?;
    Ok(true)
}

fn child_directories(path: &Path) -> Result<Vec<String>, LifecycleError> {
    let root = path
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == ".supercov"))
        .and_then(Path::parent)
        .unwrap_or(path);
    reject_linked_ancestors(root, path, true)?;
    let entries = match crate::fault::io("store-list", io::ErrorKind::PermissionDenied, || {
        fs::read_dir(path)
    }) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_error(path, source)),
    };
    let mut names = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_error(path, source))?;
        let file_type = fs::file_type(&entry).map_err(|source| io_error(&entry.path(), source))?;
        if file_type.is_symlink() {
            return Err(LifecycleError::UnsafePath(entry.path()));
        }
        if !file_type.is_dir() {
            continue;
        }
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| LifecycleError::UnsafePath(entry.path()))?;
        checked_id(&name)?;
        names.push(name);
    }
    names.sort();
    Ok(names)
}

/// Where a run's evidence is kept when Supercov could not publish it.
const FAILED_EVIDENCE: &str = ".supercov/failed-evidence";

/// Keep what a run measured when Supercov itself is what failed.
///
/// A suite pays for its measurement in wall clock, and the one that lost a run
/// to a phase reported twice had spent two and three-quarter hours on it. The
/// work directory goes when a run does not publish, and it held the only copy
/// of the evidence: nothing was left to diagnose the failure with, and nothing
/// to recover. The evidence is kept outside the work tree, where neither the
/// recovery sweep nor the next run reclaims it, and the failure says where.
///
/// Best effort by design: a run has already failed by the time this is called,
/// and being unable to keep the evidence must not replace the failure that
/// matters with one about copying it.
pub fn keep_failed_evidence(root: &Path, evidence: &Path, id: &str) -> Option<PathBuf> {
    if !evidence.is_dir() {
        return None;
    }
    let kept = root.join(FAILED_EVIDENCE).join(id);
    fs::create_dir_all(&kept).ok()?;
    let mut files = 0;
    if copy_evidence(evidence, &kept, &mut files).is_err() || files == 0 {
        // Nothing worth keeping, or nothing that could be: leave no empty
        // directory behind to puzzle over.
        let _ = fs::remove_dir_all(&kept);
        return None;
    }
    Some(kept)
}

/// Say where a failed run's evidence was kept, on the failure itself.
///
/// The user is looking at a Supercov bug at this point; the one thing they can
/// do about it is send the evidence, and they only will if the failure says
/// where it is. Returns the error unchanged when there was nothing to keep.
pub fn note_kept_evidence(root: &Path, evidence: &Path, id: &str, error: String) -> String {
    let Some(kept) = keep_failed_evidence(root, evidence, id) else {
        return error;
    };
    format!(
        "{error}\n[supercov] the tests were measured and the run could not be published; \
their evidence is kept at {} -- this is a Supercov bug, and that directory is what diagnoses it",
        kept.display()
    )
}

fn copy_evidence(source: &Path, destination: &Path, files: &mut usize) -> std::io::Result<()> {
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if fs::file_type(&entry)?.is_dir() {
            fs::create_dir_all(&target)?;
            copy_evidence(&entry.path(), &target, files)?;
        } else {
            fs::copy(entry.path(), &target)?;
            *files += 1;
        }
    }
    Ok(())
}

/// Clears what runs that died left behind, and copies nobody has used for a
/// day.
///
/// Every run asks for this as it starts, and none needs it: its own run is
/// new. So one of the runs started together does it, and the others, finding
/// it being done, go on.
pub fn recover_abandoned_runs(
    root: &Path,
    updated_at: &str,
) -> Result<Vec<String>, LifecycleError> {
    let locks = root.join(".supercov/locks");
    fs::create_dir_all(&locks).map_err(|source| io_error(&locks, source))?;
    let Some(_recovering) = take_lock(&locks.join("recover.lock"), false, || false)? else {
        return Ok(Vec::new());
    };
    remove_idle_copies(root, &locks)?;
    let mut recovered = Vec::new();
    for id in child_directories(&root.join(".supercov/work"))? {
        let Some(state) = read_state(root, &id)? else {
            continue;
        };
        if state.status.terminal() {
            finalize_published_run(root, &id)?;
            continue;
        }
        if process_exists(state.pid) {
            continue;
        }
        let workspace_name = root.file_name().unwrap_or_default();
        remove_stored_tree_deferred(
            root,
            &root.join(".supercov/work").join(&id).join(workspace_name),
        )?;
        remove_stored_tree_deferred(
            root,
            &root
                .join(".supercov/work")
                .join(&id)
                .join("run-publication"),
        )?;
        if !finalize_published_run(root, &id)? {
            remove_stored_tree_deferred(root, &root.join(".supercov/evidence").join(&id))?;
            update_run_state(
                root,
                &id,
                RunStateStatus::Abandoned,
                updated_at,
                Some(format!(
                    "Recovered after process {} exited without cleanup",
                    state.pid
                )),
            )?;
            remove_stored_tree_deferred(root, &root.join(".supercov/work").join(&id))?;
        }
        recovered.push(id);
    }
    recovered.sort();
    Ok(recovered)
}

/// Removes each copy past the first that no run holds and none has used for
/// a day. Its lock is taken first, so a copy in use is never the one removed.
fn remove_idle_copies(root: &Path, locks: &Path) -> Result<(), LifecycleError> {
    let container = crate::workspace::workspace_container(root);
    for copy in 2..=MOST_COPIES {
        let lock_path = locks.join(format!("{}.lock", copy_lock_name(copy)));
        let Ok(metadata) = fs::metadata(&lock_path) else {
            continue;
        };
        let idle = metadata
            .modified()
            .ok()
            .and_then(|used| SystemTime::now().duration_since(used).ok())
            .is_some_and(|idle| idle >= IDLE_COPY_KEPT);
        if !idle {
            continue;
        }
        let Some(_held) = take_lock(&lock_path, false, || live_owner(locks, copy).is_some())?
        else {
            continue;
        };
        // The copy and its source view.
        crate::workspace::copy_directories(&container, copy)
            .iter()
            .filter(|directory| fs::symlink_metadata(directory).is_ok())
            .try_for_each(|directory| remove_stored_tree_deferred(root, directory).map(drop))?;
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CleanupOptions {
    pub keep: usize,
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanupResult {
    pub removed_runs: Vec<String>,
    pub removed_workspaces: Vec<String>,
    pub removed_evidence: Vec<String>,
    /// Evidence kept from runs Supercov itself failed to publish. Retention
    /// leaves it alone -- it is the only copy, and the automatic path taking it
    /// is what made a lost run unrecoverable -- but `clean` is the user asking
    /// for the project's storage back, so it goes then, and the summary says so.
    pub removed_failed_evidence: Vec<String>,
    pub removed_build_cache: bool,
}

pub fn cleanup_storage_locked(
    root: &Path,
    options: CleanupOptions,
    remove_build_cache: bool,
) -> Result<CleanupResult, LifecycleError> {
    let runs_root = root.join(".supercov/runs");
    let work_root = root.join(".supercov/work");
    let evidence_root = root.join(".supercov/evidence");
    let published = child_directories(&runs_root)?;
    let work = child_directories(&work_root)?;
    let evidence = child_directories(&evidence_root)?;
    let mut ids = published
        .iter()
        .chain(&work)
        .chain(&evidence)
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    ids.sort_by(|left, right| right.cmp(left));
    let mut active = BTreeSet::new();
    for id in &ids {
        if read_state(root, id)?.is_some_and(|state| !state.status.terminal()) {
            active.insert(id.clone());
        }
    }
    let mut retention_order = published
        .iter()
        .filter(|id| !active.contains(*id))
        .map(|id| (published_started_at(root, id), id.clone()))
        .collect::<Vec<_>>();
    retention_order.sort_by(|left, right| right.0.cmp(&left.0).then_with(|| right.1.cmp(&left.1)));
    let retained = retention_order
        .into_iter()
        .take(options.keep)
        .map(|(_, id)| id)
        .collect::<BTreeSet<_>>();
    let mut result = CleanupResult {
        removed_runs: Vec::new(),
        removed_workspaces: Vec::new(),
        removed_evidence: Vec::new(),
        removed_failed_evidence: Vec::new(),
        removed_build_cache: false,
    };
    for id in ids {
        if active.contains(&id) {
            continue;
        }
        let has_run = published.contains(&id);
        let remove_history = has_run && !retained.contains(&id);
        // A run whose state is not terminal is active, and passed over above.
        if work.contains(&id) {
            result.removed_workspaces.push(id.clone());
            if !options.dry_run {
                remove_stored_tree_deferred(root, &work_root.join(&id))?;
            }
        }
        if evidence.contains(&id) && (!has_run || remove_history) {
            result.removed_evidence.push(id.clone());
            if !options.dry_run {
                remove_stored_tree_deferred(root, &evidence_root.join(&id))?;
            }
        }
        if remove_history {
            result.removed_runs.push(id.clone());
            if !options.dry_run {
                remove_stored_tree_deferred(root, &runs_root.join(&id))?;
            }
        }
    }
    // Only a full `clean` reclaims kept evidence. `--keep N` is a trim, and the
    // evidence of a run Supercov failed to publish is the last thing a user
    // trimming their history means to lose; a retention pass would take it
    // within one run of keeping it.
    if remove_build_cache && options.keep == 0 && active.is_empty() {
        let kept_root = root.join(FAILED_EVIDENCE);
        for id in child_directories(&kept_root)? {
            result.removed_failed_evidence.push(id.clone());
            if !options.dry_run {
                remove_stored_tree_deferred(root, &kept_root.join(&id))?;
            }
        }
    }
    let container = crate::workspace::workspace_container(root);
    // What runs keep between them: the Docker image registry, among others.
    let shared = root.join(".supercov/cache");
    let mut caches = Vec::new();
    if remove_build_cache && active.is_empty() {
        if owned_workspace_container(root) {
            caches.push(container);
        }
        if fs::symlink_metadata(&shared).is_ok() {
            caches.push(shared);
        }
    }
    result.removed_build_cache = !caches.is_empty();
    if !options.dry_run {
        for cache in caches {
            remove_stored_tree_deferred(root, &cache)?;
        }
    }
    Ok(result)
}

pub fn clean_storage(
    root: &Path,
    options: CleanupOptions,
    updated_at: &str,
) -> Result<CleanupResult, LifecycleError> {
    let lock_id = format!("clean-{}-{}", std::process::id(), unique_name());
    let mut lock = ProjectLock::acquire_alone(root, &lock_id, updated_at)?;
    recover_abandoned_runs(root, updated_at)?;
    let result = cleanup_storage_locked(root, options, true);
    lock.release()?;
    result
}

/// The metadata of a run `id` whose evidence is `bytes` long, for tests.
#[cfg(test)]
pub(crate) fn test_run_metadata(id: &str, bytes: u64) -> RunMetadata {
    use crate::run_store::{RawEvidenceMetadata, RunFingerprint, RunIntegrity};

    RunMetadata {
        id: id.into(),
        started_at: "2026-01-01T00:00:00Z".into(),
        duration_ms: 1.0,
        command: vec!["test".into()],
        test_exit_code: Some(0),
        integrity: RunIntegrity {
            schema_version: 2,
            instrumenter_version: "rust".into(),
            git: None,
            fingerprint: RunFingerprint {
                algorithm: "sha256".into(),
                source: "0".repeat(64),
                tests: "0".repeat(64),
                dependencies: "0".repeat(64),
                configuration: "0".repeat(64),
                instrumenter: "0".repeat(64),
                execution: "0".repeat(64),
                combined: "0".repeat(64),
                source_files: 1,
                test_files: 1,
            },
            stale: None,
            stale_reasons: None,
        },
        raw_evidence: RawEvidenceMetadata {
            schema_version: 2,
            format: "supercov-evidence-archive".into(),
            file: "evidence.raw.gz".into(),
            files: 1,
            uncompressed_bytes: bytes,
            compressed_bytes: bytes,
        },
        isolated_build: None,
        instrumented_build_cache: None,
        timings: None,
        merged: None,
        parents: None,
        source_roots: None,
        copy: None,
        merged_across: None,
        recorded_by: None,
        companions: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> PathBuf {
        let root = std::env::temp_dir().join(format!("supercov-lifecycle-{}", unique_name()));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/index.js"), "user source").unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn removing_a_stored_tree_never_follows_a_link_out_of_it() {
        // A report from supergateway: a worktree's node_modules linked to the
        // main checkout's, and the main checkout's vanished mid-run. Supercov
        // was not the cause (another process removed it), but its removal and
        // sweep must never reach through a link into what the link points at.
        let root = project();
        let outside = std::env::temp_dir().join(format!("supercov-outside-{}", unique_name()));
        fs::create_dir_all(outside.join("tsx/dist")).unwrap();
        fs::write(outside.join("tsx/dist/loader.mjs"), "the dependency").unwrap();
        let tree = root.join(".supercov/work/run_links");
        fs::create_dir_all(tree.join("mirror")).unwrap();
        std::os::unix::fs::symlink(&outside, tree.join("node_modules")).unwrap();
        // The workspace's node_modules is a real directory of per-package links.
        std::os::unix::fs::symlink(outside.join("tsx"), tree.join("mirror/tsx")).unwrap();
        // And a stored tree that is itself a link.
        std::os::unix::fs::symlink(&outside, root.join(".supercov/work/run_linked_root")).unwrap();
        for target in [tree.clone(), root.join(".supercov/work/run_linked_root")] {
            remove_stored_tree_deferred(&root, &target)
                .unwrap()
                .unwrap();
        }
        assert!(sweep_trash(&root).unwrap() >= 2);
        assert!(!tree.exists());
        assert_eq!(
            fs::read_to_string(outside.join("tsx/dist/loader.mjs")).unwrap(),
            "the dependency"
        );
        fs::remove_dir_all(&outside).unwrap();
        fs::remove_dir_all(&root).unwrap();
    }

    fn kept_evidence(root: &Path, id: &str) -> PathBuf {
        let evidence = root.join(".supercov/work").join(id).join("python/evidence");
        fs::create_dir_all(&evidence).unwrap();
        fs::write(evidence.join("main.jsonl"), "what the run saw").unwrap();
        keep_failed_evidence(root, &evidence, id).expect("the evidence")
    }

    fn state(root: &Path, id: &str, status: RunStateStatus, pid: u32) -> RunState {
        RunState {
            id: id.into(),
            pid,
            root: root.display().to_string(),
            workspace: root.join("dist").display().to_string(),
            started_at: "start".into(),
            updated_at: "update".into(),
            status,
            signal: None,
            error: None,
        }
    }

    fn metadata(id: &str, bytes: u64) -> RunMetadata {
        test_run_metadata(id, bytes)
    }

    fn evidence(root: &Path) -> (PathBuf, u64) {
        use crate::evidence_archive::{EvidenceArchiveEntry, write_archive};
        let mut entries = ["manifest.json", "frontend.json", "coverage-model.json"]
            .into_iter()
            .map(|path| EvidenceArchiveEntry {
                path: path.into(),
                contents: b"{}".to_vec(),
            })
            .collect::<Vec<_>>();
        let inputs =
            crate::source_capture::capture(root, "javascript", [PathBuf::from("src/index.js")])
                .unwrap();
        entries = crate::source_capture::append(entries, &inputs).unwrap();
        let path = root.join("evidence.gz");
        let metadata = write_archive(entries, &path).unwrap();
        (path, metadata.compressed_bytes)
    }

    /// Where the file system keeps no locks, the owners written beside them
    /// decide: a run goes ahead, and cleaning, which wants the store alone,
    /// is refused while another process owns a copy.
    #[cfg(unix)]
    #[test]
    fn without_file_locks_the_owners_beside_them_decide() {
        let root = project();
        crate::fault::during("lock-unsupported", || {
            drop(ProjectLock::acquire(&root, "run_a", "now").unwrap());
            drop(ProjectLock::acquire_alone(&root, "run_b", "now").unwrap());
            // A copy owned by a process that is still there (init's).
            fs::write(
                root.join(".supercov/locks/active.json"),
                serde_json::to_vec(&LockOwner {
                    run_id: "run_c".into(),
                    pid: 1,
                    started_at: "now".into(),
                })
                .unwrap(),
            )
            .unwrap();
            assert!(ProjectLock::acquire_alone(&root, "run_d", "now").is_err());
        });
        fs::remove_dir_all(root).unwrap();
    }

    /// Each store operation whose system call fails: a write retries a name
    /// taken and cleans up after one it could not finish, the sweep passes
    /// over what is already gone and stops at anything else, and publication
    /// refuses a copy that is not the evidence it hashed.
    #[test]
    fn a_failing_system_call_in_the_store_is_retried_skipped_or_reported() {
        use crate::fault::during;
        let root = project();
        let state = root.join(".supercov/state.json");
        during("atomic-write-create=1", || {
            atomic_write(&root, &state, b"one")
        })
        .unwrap();
        assert_eq!(fs::read(&state).unwrap(), b"one");
        assert!(during("atomic-write-sync", || atomic_write(&root, &state, b"two")).is_err());
        assert_eq!(fs::read(&state).unwrap(), b"one");
        let leftovers = fs::read_dir(state.parent().unwrap())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .ends_with(".tmp")
            })
            .count();
        assert_eq!(leftovers, 0, "a write it could not finish leaves nothing");

        let trashed = || {
            let owned = root.join(".supercov/evidence/run");
            fs::create_dir_all(&owned).unwrap();
            fs::write(owned.join("hit"), "hit").unwrap();
            remove_stored_tree_deferred(&root, &owned).unwrap().unwrap()
        };
        // Any error but one that says the entry is gone (13 is EACCES).
        let denied = ":13";
        let entry = trashed();
        // Gone already, by hand or another sweeper: as swept as removed here.
        assert_eq!(during("trash-entry", || sweep_trash(&root)).unwrap(), 0);
        assert_eq!(during("trash-remove", || sweep_trash(&root)).unwrap(), 0);
        for fault in ["trash-list", "trash-lock"] {
            assert!(during(fault, || sweep_trash(&root)).is_err(), "{fault}");
        }
        for fault in ["trash-entry", "trash-remove"] {
            assert!(
                during(&format!("{fault}{denied}"), || sweep_trash(&root)).is_err(),
                "{fault}"
            );
        }
        assert!(entry.exists());
        assert_eq!(sweep_trash(&root).unwrap(), 1);

        assert!(
            during("store-ignore", || ProjectLock::acquire(
                &root, "run_a", "now"
            ))
            .is_err()
        );
        assert!(during("store-list", || recover_abandoned_runs(&root, "now")).is_err());

        let id = "2026-01-01T00-00-00-000Z";
        let (evidence, bytes) = evidence(&root);
        assert!(matches!(
            during("published-copy-differs", || publish_run(
                &root,
                &metadata(id, bytes),
                &evidence
            )),
            Err(LifecycleError::EvidenceChanged)
        ));
        assert!(!root.join(".supercov/runs").join(id).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn defers_only_owned_storage_and_sweeps_without_touching_source() {
        let root = project();
        let owned = root.join(".supercov/evidence/run");
        fs::create_dir_all(&owned).unwrap();
        fs::write(owned.join("hit"), "hit").unwrap();
        let trash = remove_stored_tree_deferred(&root, &owned).unwrap().unwrap();
        assert!(!owned.exists());
        assert!(trash.exists());
        assert!(matches!(
            remove_stored_tree_deferred(&root, &root.join("src")),
            Err(LifecycleError::UnsafePath(_))
        ));
        assert_eq!(sweep_trash(&root).unwrap(), 1);
        assert!(root.join("src/index.js").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn refuses_linked_storage_ancestors_instead_of_renaming_external_data() {
        use std::os::unix::fs::symlink;

        let root = project();
        let outside = project();
        fs::create_dir_all(root.join(".supercov")).unwrap();
        fs::create_dir_all(outside.join("run")).unwrap();
        fs::write(outside.join("run/user.txt"), "user").unwrap();
        symlink(&outside, root.join(".supercov/evidence")).unwrap();
        assert!(matches!(
            remove_stored_tree_deferred(&root, &root.join(".supercov/evidence/run")),
            Err(LifecycleError::UnsafePath(_))
        ));
        assert_eq!(
            fs::read_to_string(outside.join("run/user.txt")).unwrap(),
            "user"
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    /// Evidence that analyses -- the analysable fixture run -- to publish as
    /// `id`, from outside the project's runs.
    fn analysable_evidence(root: &Path, id: &str) -> (PathBuf, RunMetadata) {
        let source = project();
        let directory = crate::run_store::create_analyzable_test_run(&source, "source");
        let evidence = root.join(format!("{id}.evidence.gz"));
        fs::copy(directory.join("evidence.raw.gz"), &evidence).unwrap();
        let mut metadata: RunMetadata =
            serde_json::from_slice(&fs::read(directory.join("run.json")).unwrap()).unwrap();
        metadata.id = id.into();
        fs::remove_dir_all(source).unwrap();
        (evidence, metadata)
    }

    /// The published run as a query reads it.
    fn stored(root: &Path, id: &str) -> crate::run_store::StoredRun {
        let directory = root.join(".supercov/runs").join(id);
        crate::run_store::StoredRun {
            id: id.into(),
            evidence_path: directory.join("evidence.raw.gz"),
            metadata_path: directory.join("run.json"),
            query_index_path: directory.join(crate::run_store::RUST_QUERY_INDEX_FILE),
            metadata: serde_json::from_slice(&fs::read(directory.join("run.json")).unwrap())
                .unwrap(),
            directory,
        }
    }

    #[test]
    fn publication_writes_the_query_index_a_first_query_would_have_built() {
        // Written beside staged evidence and renamed with it, the index must be
        // the one the first query builds from the published evidence -- the
        // same bytes, valid under the published run's identity.
        let root = project();
        let id = "2026-01-01T00-00-00-100Z";
        let (evidence, metadata) = analysable_evidence(&root, id);
        let published = publish_run(&root, &metadata, &evidence).unwrap();
        let index = published.join(crate::run_store::RUST_QUERY_INDEX_FILE);
        assert!(index.is_file(), "publication wrote the index");
        let run = stored(&root, id);
        assert!(
            crate::run_store::open_existing_query_index(&run)
                .unwrap()
                .is_some(),
            "and it is valid for the published run"
        );
        // The report's statement spans come from the same analysis, and are
        // what analysing the published evidence again would give.
        let analysed = crate::source_manifest::coverage(&run).unwrap();
        assert_eq!(
            crate::run_store::read_statement_spans(&run).expect("publication wrote the spans"),
            crate::run_store::statement_spans(&analysed)
        );
        // So is what an assessment reads.
        assert_eq!(
            crate::run_store::read_assessment_input(&run).expect("publication wrote the input"),
            crate::assertion_coverage::assessment_input(&analysed)
        );
        let written = fs::read(&index).unwrap();
        fs::remove_file(&index).unwrap();
        crate::run_store::open_or_rebuild_query_index(&run).unwrap();
        assert_eq!(
            fs::read(&index).unwrap(),
            written,
            "a query rebuilds the same bytes"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn evidence_that_will_not_analyse_is_published_as_it_was_before() {
        // Publication used to tolerate evidence it could not analyse and leave
        // the first query to report why. It still publishes it, writes neither
        // index nor summary, and the first query fails as it did.
        let root = project();
        let id = "2026-01-01T00-00-00-200Z";
        let (evidence, bytes) = evidence(&root);
        let published = publish_run(&root, &metadata(id, bytes), &evidence).unwrap();
        assert!(published.join("run.json").is_file());
        assert!(published.join("source-manifest.cache.json").is_file());
        assert!(
            !published
                .join(crate::run_store::RUST_QUERY_INDEX_FILE)
                .exists()
        );
        assert!(
            !published
                .join(crate::source_manifest::EXECUTIONS_FILE)
                .exists()
        );
        let run = stored(&root, id);
        let Err(query) = crate::run_store::open_or_rebuild_query_index(&run) else {
            panic!("the first query cannot analyse it either");
        };
        let direct = crate::run_store::analyze_stored_run(&run).expect_err("unanalysable");
        assert_eq!(
            query.to_string(),
            direct.to_string(),
            "and says why in the same words"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_merged_run_is_indexed_by_its_first_query() {
        // Publication does not analyse a merged run; its first query does.
        let root = project();
        let id = "2026-01-01T00-00-00-300Z";
        let (evidence, mut metadata) = analysable_evidence(&root, id);
        metadata.merged = Some(true);
        let published = publish_run(&root, &metadata, &evidence).unwrap();
        let index = published.join(crate::run_store::RUST_QUERY_INDEX_FILE);
        assert!(!index.exists());
        crate::run_store::open_or_rebuild_query_index(&stored(&root, id)).unwrap();
        assert!(index.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_index_that_could_not_be_written_is_built_by_the_first_query() {
        // A failed index write loses nothing: publication succeeds without a
        // partial file, and the first query builds the index as it used to.
        // The other files derived from the evidence are as disposable.
        let root = project();
        let id = "2026-01-01T00-00-00-400Z";
        let (evidence, metadata) = analysable_evidence(&root, id);
        let published = publish_run_with_fault(
            &root,
            &metadata,
            &evidence,
            Archived::default(),
            Some(RunPublicationFault::DerivedWrites),
        )
        .unwrap();
        let index = published.join(crate::run_store::RUST_QUERY_INDEX_FILE);
        assert!(!index.exists(), "no partial index is left");
        assert!(
            !published
                .join(crate::run_store::STATEMENT_SPANS_FILE)
                .exists()
        );
        assert!(
            !published
                .join(crate::run_store::ASSESSMENT_INPUT_FILE)
                .exists()
        );
        assert!(published.join("run.json").is_file());
        crate::run_store::open_or_rebuild_query_index(&stored(&root, id)).unwrap();
        assert!(index.is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn publishes_both_required_files_with_one_visible_rename() {
        let root = project();
        let id = "2026-01-01T00-00-00-000Z";
        let (evidence, bytes) = evidence(&root);
        let published = publish_run(&root, &metadata(id, bytes), &evidence).unwrap();
        assert!(published.join("run.json").is_file());
        assert_eq!(
            fs::read(published.join("evidence.raw.gz")).unwrap(),
            fs::read(&evidence).unwrap()
        );
        assert!(published.join("source-manifest.cache.json").is_file());
        assert!(matches!(
            publish_run(&root, &metadata(id, bytes), &evidence),
            Err(LifecycleError::PublicationExists(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    /// Evidence whose length is not the one the run recorded is refused and
    /// leaves nothing published; a staging directory left by an interrupted
    /// attempt is cleared before the next.
    #[test]
    fn publication_refuses_evidence_of_another_length_and_clears_old_staging() {
        let root = project();
        let id = "2026-01-01T00-00-00-000Z";
        let (evidence, bytes) = evidence(&root);
        let staging = root.join(".supercov/work").join(id).join("run-publication");
        assert!(matches!(
            publish_run(&root, &metadata(id, bytes + 1), &evidence),
            Err(LifecycleError::EvidenceLength { expected, actual }) if expected == bytes + 1 && actual == bytes
        ));
        assert!(!root.join(".supercov/runs").join(id).exists());
        assert!(!staging.join("evidence.raw.gz").exists());
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("leftover"), b"from an interrupted attempt").unwrap();
        let published = publish_run(&root, &metadata(id, bytes), &evidence).unwrap();
        assert!(!published.join("leftover").exists());
        fs::remove_dir_all(root).unwrap();
    }

    /// Only Supercov's own stored trees are removed: a path in the project,
    /// in the trash, or the store itself is refused; a missing one is done.
    /// What is copied or hashed is a regular file.
    #[cfg(unix)]
    #[test]
    fn only_stored_trees_are_removed_and_only_files_copied() {
        let root = project();
        fs::create_dir_all(root.join(".supercov/runs/r1")).unwrap();
        let moved = remove_stored_tree_deferred(&root, Path::new(".supercov/runs/r1")).unwrap();
        assert!(moved.is_some_and(|path| path.starts_with(root.join(TRASH))));
        assert_eq!(
            remove_stored_tree_deferred(&root, Path::new(".supercov/runs/gone")).unwrap(),
            None
        );
        fs::create_dir_all(root.join(TRASH).join("old")).unwrap();
        for target in [
            root.join("src"),
            root.join(TRASH).join("old"),
            root.join(".supercov"),
        ] {
            assert!(
                matches!(
                    remove_stored_tree_deferred(&root, &target),
                    Err(LifecycleError::UnsafePath(_))
                ),
                "{}",
                target.display()
            );
        }
        assert!(!lexical_descendant(&root, &root));
        assert!(!lexical_descendant(&root, &root.join("a/../b")));
        assert!(lexical_descendant(&root, &root.join("a/b")));
        assert_eq!(
            absolute_root(Path::new("relative")).unwrap(),
            std::env::current_dir().unwrap().join("relative")
        );
        assert!(process_exists(std::process::id()));
        assert!(!process_exists(0));
        assert!(!process_exists(u32::MAX));
        let directory = root.join("src");
        assert!(matches!(
            copy_regular_file(&directory, &root.join("copy")),
            Err(LifecycleError::UnsafePath(_))
        ));
        fs::write(root.join("archive"), "evidence").unwrap();
        for (fault, copy) in [("clone", "cloned"), ("", "copied")] {
            let copied = crate::fault::during(fault, || {
                copy_regular_file(&root.join("archive"), &root.join(copy))
            });
            assert_eq!(copied.unwrap(), 8);
            assert_eq!(fs::read_to_string(root.join(copy)).unwrap(), "evidence");
        }
        assert!(matches!(
            file_sha256(&directory),
            Err(LifecycleError::UnsafePath(_))
        ));
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A copy whose owner file names a live process that holds no lock -- a
    /// release that locked nothing -- still counts: cleaning is refused
    /// naming it, and a run takes the next copy.
    #[cfg(unix)]
    #[test]
    fn an_owner_without_a_lock_still_holds_its_copy() {
        let root = project();
        let locks = root.join(".supercov/locks");
        fs::create_dir_all(&locks).unwrap();
        let foreign = LockOwner {
            run_id: "run_foreign".into(),
            pid: 1,
            started_at: "earlier".into(),
        };
        fs::write(
            locks.join(format!("{}.json", copy_lock_name(1))),
            serde_json::to_vec(&foreign).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            ProjectLock::acquire_alone(&root, "clean-1", "now"),
            Err(LifecycleError::ActiveRun { pid: 1, .. })
        ));
        let mut lock = ProjectLock::acquire(&root, "run_mine", "now").unwrap();
        assert_eq!(lock.copy(), 2);
        assert!(lock.protects(&root));
        assert!(!lock.protects(&root.join("other")));
        lock.release().unwrap();
        lock.release().unwrap();
        assert!(!lock.protects(&root));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn final_rename_failure_exposes_no_run_and_removes_staging() {
        let root = project();
        let id = "2026-01-01T00-00-00-000Z";
        let (evidence, bytes) = evidence(&root);
        let error = publish_run_with_fault(
            &root,
            &metadata(id, bytes),
            &evidence,
            Archived::default(),
            Some(RunPublicationFault::FinalRename),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("injected final run publication rename failure")
        );
        assert!(!root.join(".supercov/runs").join(id).exists());
        assert!(
            !root
                .join(".supercov/work")
                .join(id)
                .join("run-publication")
                .exists()
        );
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn malformed_assertion_inputs_expose_no_partial_run() {
        use crate::evidence_archive::{read_archive, write_archive};
        let root = project();
        let id = "2026-01-01T00-00-00-000Z";
        let (evidence, _) = evidence(&root);
        let mut entries = read_archive(&evidence).unwrap();
        entries
            .iter_mut()
            .find(|e| e.path == crate::source_capture::ARCHIVE_PATH)
            .unwrap()
            .contents = b"{broken".to_vec();
        let raw = write_archive(entries, &evidence).unwrap();
        let error = publish_run(&root, &metadata(id, raw.compressed_bytes), &evidence).unwrap_err();
        assert!(error.to_string().contains("source manifest publication"));
        assert!(!root.join(".supercov/runs").join(id).exists());
        assert!(
            !root
                .join(".supercov/work")
                .join(id)
                .join("run-publication")
                .exists()
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn recovers_dead_unpublished_and_fully_published_runs_from_derived_paths() {
        let root = project();
        let dead = "2026-01-01T00-00-00-000Z";
        let published = "2026-01-02T00-00-00-000Z";
        for id in [dead, published] {
            fs::create_dir_all(
                root.join(".supercov/work")
                    .join(id)
                    .join(root.file_name().unwrap()),
            )
            .unwrap();
            fs::create_dir_all(root.join(".supercov/evidence").join(id)).unwrap();
            write_run_state(&root, &state(&root, id, RunStateStatus::Testing, u32::MAX)).unwrap();
        }
        let (evidence, bytes) = evidence(&root);
        publish_run(&root, &metadata(published, bytes), &evidence).unwrap();
        assert_eq!(
            recover_abandoned_runs(&root, "recovered").unwrap(),
            [dead, published]
        );
        assert!(!root.join(".supercov/work").join(dead).exists());
        assert!(!root.join(".supercov/work").join(published).exists());
        assert!(root.join(".supercov/runs").join(published).exists());
        assert!(root.join("src/index.js").exists());
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A run whose process still lives is left to finish; a run directory
    /// with no state is passed over; and a link where a run's directory
    /// belongs is refused, while a file there is not a run.
    #[test]
    fn a_live_run_is_left_alone_and_a_link_among_runs_is_refused() {
        let root = project();
        let live = "2026-01-03T00-00-00-000Z";
        let stateless = "2026-01-04T00-00-00-000Z";
        for id in [live, stateless] {
            fs::create_dir_all(root.join(".supercov/work").join(id)).unwrap();
        }
        write_run_state(
            &root,
            &state(&root, live, RunStateStatus::Testing, std::process::id()),
        )
        .unwrap();
        fs::write(root.join(".supercov/work/notes.txt"), "not a run").unwrap();
        assert!(
            recover_abandoned_runs(&root, "recovered")
                .unwrap()
                .is_empty()
        );
        assert!(root.join(".supercov/work").join(live).exists());
        assert!(root.join(".supercov/work").join(stateless).exists());
        let mut names = child_directories(&root.join(".supercov/work")).unwrap();
        names.sort();
        assert_eq!(names, [live, stateless]);
        assert!(
            child_directories(&root.join(".supercov/absent"))
                .unwrap()
                .is_empty()
        );
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(root.join("src"), root.join(".supercov/work/linked"))
                .unwrap();
            assert!(matches!(
                child_directories(&root.join(".supercov/work")),
                Err(LifecycleError::UnsafePath(path)) if path.ends_with("linked")
            ));
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// Evidence goes with its run: an orphan's always, a kept run's never.
    /// A full clean takes kept evidence and the build cache only once no
    /// run is active.
    #[test]
    fn evidence_goes_with_its_run_and_a_clean_waits_for_active_runs() {
        let root = project();
        let (old, new, orphan, active) = (
            "2026-01-01T00-00-00-000Z",
            "2026-01-02T00-00-00-000Z",
            "2026-01-03T00-00-00-000Z",
            "2026-01-04T00-00-00-000Z",
        );
        for id in [old, new] {
            fs::create_dir_all(root.join(".supercov/runs").join(id)).unwrap();
        }
        for id in [old, new, orphan] {
            fs::create_dir_all(root.join(".supercov/evidence").join(id)).unwrap();
        }
        write_run_state(
            &root,
            &state(&root, active, RunStateStatus::Testing, std::process::id()),
        )
        .unwrap();
        kept_evidence(&root, "2025-12-31T00-00-00-000Z");
        fs::create_dir_all(root.join(".supercov/cache")).unwrap();
        let clean = |keep: usize| {
            cleanup_storage_locked(
                &root,
                CleanupOptions {
                    keep,
                    dry_run: true,
                },
                true,
            )
            .unwrap()
        };
        let trim = clean(1);
        assert_eq!(trim.removed_runs, [old]);
        assert_eq!(trim.removed_evidence, [orphan, old]);
        assert!(trim.removed_failed_evidence.is_empty());
        assert!(!trim.removed_build_cache, "a run is active");
        let full = clean(0);
        assert!(full.removed_failed_evidence.is_empty(), "a run is active");
        assert!(!full.removed_build_cache);
        write_run_state(
            &root,
            &state(&root, active, RunStateStatus::Complete, std::process::id()),
        )
        .unwrap();
        let idle = clean(0);
        assert_eq!(idle.removed_failed_evidence, ["2025-12-31T00-00-00-000Z"]);
        assert!(idle.removed_build_cache);
        assert!(idle.removed_workspaces.contains(&active.to_owned()));
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn retention_is_deterministic_dry_run_safe_and_preserves_active_work() {
        let root = project();
        let ids = [
            "2026-01-01T00-00-00-000Z",
            "2026-01-02T00-00-00-000Z",
            "2026-01-03T00-00-00-000Z",
        ];
        for id in ids {
            fs::create_dir_all(root.join(".supercov/runs").join(id)).unwrap();
            write_run_state(
                &root,
                &state(&root, id, RunStateStatus::Complete, std::process::id()),
            )
            .unwrap();
        }
        let active = "2025-12-31T00-00-00-000Z";
        write_run_state(
            &root,
            &state(&root, active, RunStateStatus::Testing, std::process::id()),
        )
        .unwrap();
        let preview = cleanup_storage_locked(
            &root,
            CleanupOptions {
                keep: 1,
                dry_run: true,
            },
            false,
        )
        .unwrap();
        assert_eq!(preview.removed_runs, [ids[1], ids[0]]);
        assert!(
            ids.iter()
                .all(|id| root.join(".supercov/runs").join(id).exists())
        );
        let result = cleanup_storage_locked(
            &root,
            CleanupOptions {
                keep: 1,
                dry_run: false,
            },
            false,
        )
        .unwrap();
        assert_eq!(result, preview);
        assert!(root.join(".supercov/work").join(active).exists());
        assert!(root.join(".supercov/runs").join(ids[2]).exists());
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// A full clean previews exactly what it removes -- runs, finished
    /// workspaces, evidence no run published, kept failed evidence and the
    /// build cache -- and a preview removes none of it.
    #[test]
    fn a_full_clean_previews_everything_it_removes_and_the_preview_removes_nothing() {
        let root = project();
        let (evidence, bytes) = evidence(&root);
        publish_run(&root, &metadata("run_published", bytes), &evidence).unwrap();
        write_run_state(
            &root,
            &state(
                &root,
                "run_finished",
                RunStateStatus::Complete,
                std::process::id(),
            ),
        )
        .unwrap();
        let stored = [
            root.join(".supercov/evidence/run_orphaned"),
            root.join(FAILED_EVIDENCE).join("run_failed"),
            root.join(".supercov/cache/docker-images"),
        ];
        for directory in &stored {
            fs::create_dir_all(directory).unwrap();
        }
        let clean = |dry_run| {
            cleanup_storage_locked(&root, CleanupOptions { keep: 0, dry_run }, true).unwrap()
        };
        let preview = clean(true);
        assert_eq!(preview.removed_runs, ["run_published"]);
        assert_eq!(
            preview.removed_workspaces,
            ["run_published", "run_finished"]
        );
        assert_eq!(preview.removed_evidence, ["run_orphaned"]);
        assert_eq!(preview.removed_failed_evidence, ["run_failed"]);
        assert!(preview.removed_build_cache);
        assert!(root.join(".supercov/runs/run_published").exists());
        assert!(root.join(".supercov/work/run_finished").exists());
        assert!(stored.iter().all(|directory| directory.exists()));

        assert_eq!(clean(false), preview);
        assert!(!root.join(".supercov/runs/run_published").exists());
        assert!(!root.join(".supercov/work/run_finished").exists());
        assert!(stored.iter().all(|directory| !directory.exists()));
        assert!(
            root.join("src/index.js").exists(),
            "the project's own files stay"
        );
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn retention_keeps_the_newest_runs_when_run_ids_disagree_with_recency() {
        let root = project();
        let (evidence, bytes) = evidence(&root);
        // Run IDs are random hex, so ordering them as strings says nothing about
        // time. The newest run here carries the smallest ID and must survive.
        for (id, started_at) in [
            ("run_ffffffffffffffff", "2026-01-01T00:00:00Z"),
            ("run_7777777777777777", "2026-05-01T00:00:00Z"),
            ("run_0000000000000001", "2026-09-01T00:00:00Z"),
        ] {
            let mut run = metadata(id, bytes);
            run.started_at = started_at.into();
            publish_run(&root, &run, &evidence).unwrap();
        }
        let result = cleanup_storage_locked(
            &root,
            CleanupOptions {
                keep: 1,
                dry_run: false,
            },
            false,
        )
        .unwrap();
        assert_eq!(
            result.removed_runs,
            ["run_ffffffffffffffff", "run_7777777777777777"]
        );
        assert!(
            root.join(".supercov/runs")
                .join("run_0000000000000001")
                .exists()
        );
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn direct_runs_initialize_store_ignore_without_overwriting_existing_rules() {
        let root = project();
        let ignore = root.join(".supercov/.gitignore");
        let mut lock = ProjectLock::acquire(&root, "first", "start").unwrap();
        assert_eq!(fs::read_to_string(&ignore).unwrap(), "*\n");
        lock.release().unwrap();
        fs::write(&ignore, "custom\n").unwrap();
        let mut lock = ProjectLock::acquire(&root, "second", "start").unwrap();
        assert_eq!(fs::read_to_string(&ignore).unwrap(), "custom\n");
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    fn owner_file(root: &Path, copy: usize) -> PathBuf {
        root.join(".supercov/locks")
            .join(format!("{}.json", copy_lock_name(copy)))
    }

    #[test]
    fn runs_at_once_each_hold_a_copy_and_the_lowest_free_is_taken_next() {
        let root = project();
        let mut first = ProjectLock::acquire(&root, "first", "start").unwrap();
        let mut second = ProjectLock::acquire(&root, "second", "start").unwrap();
        let third = ProjectLock::acquire(&root, "third", "start").unwrap();
        assert_eq!((first.copy(), second.copy(), third.copy()), (1, 2, 3));
        assert_eq!(read_owner(&owner_file(&root, 2)).unwrap().run_id, "second");

        // A copy given back is the next one taken, and names nobody.
        second.release().unwrap();
        assert!(!owner_file(&root, 2).exists());
        let again = ProjectLock::acquire(&root, "fourth", "start").unwrap();
        assert_eq!(again.copy(), 2);

        // Dropped without a release, as a run that panicked is.
        drop(third);
        first.release().unwrap();
        assert_eq!(
            ProjectLock::acquire(&root, "fifth", "start")
                .unwrap()
                .copy(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn many_runs_starting_in_the_same_instant_never_share_a_copy() {
        let root = project();
        let start = std::sync::Arc::new(std::sync::Barrier::new(24));
        let holds = (0..24)
            .map(|each| {
                let root = root.clone();
                let start = std::sync::Arc::clone(&start);
                std::thread::spawn(move || {
                    start.wait();
                    let lock =
                        ProjectLock::acquire(&root, &format!("run-{each}"), "start").unwrap();
                    // Recovery is asked for by every one of them, and done by one.
                    recover_abandoned_runs(&root, "start").unwrap();
                    lock
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .collect::<Vec<_>>();
        let mut copies = holds.iter().map(ProjectLock::copy).collect::<Vec<_>>();
        copies.sort_unstable();
        assert_eq!(copies, (1..=24).collect::<Vec<_>>());
        drop(holds);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn no_run_starts_while_the_store_is_cleaned_and_none_is_cleaned_under() {
        let root = project();
        let mut cleaning = ProjectLock::acquire_alone(&root, "clean", "start").unwrap();
        assert!(matches!(
            ProjectLock::acquire(&root, "run", "start"),
            Err(LifecycleError::Cleaning)
        ));
        cleaning.release().unwrap();
        let _first = ProjectLock::acquire(&root, "first", "start").unwrap();
        let _second = ProjectLock::acquire(&root, "second", "start").unwrap();
        match ProjectLock::acquire_alone(&root, "clean", "start") {
            Err(LifecycleError::ActiveRun { run_id, pid }) => {
                assert_eq!((run_id.as_str(), pid), ("first", std::process::id()));
            }
            other => panic!("cleaning under two runs: {:?}", other.map(|_| ())),
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleaning_under_a_run_that_has_not_written_its_name_names_a_starting_run() {
        let root = project();
        let mut run = ProjectLock::acquire(&root, "run", "start").unwrap();
        // The moment between taking its share of the store and naming itself.
        fs::remove_file(owner_file(&root, run.copy())).unwrap();
        let refused = ProjectLock::acquire_alone(&root, "clean", "start").map(|_| ());
        assert!(
            matches!(refused, Err(LifecycleError::StartingRun)),
            "{refused:?}"
        );
        assert!(
            refused
                .unwrap_err()
                .to_string()
                .contains("is active in this project")
        );
        run.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn a_run_of_a_release_that_locked_nothing_keeps_its_copy() {
        // 4.4.0 wrote `active.json` and held no lock. Its run is alive for as
        // long as its process is; the process that started this test stands
        // in for it.
        let root = project();
        let locks = root.join(".supercov/locks");
        fs::create_dir_all(&locks).unwrap();
        let theirs = std::os::unix::process::parent_id();
        fs::write(
            owner_file(&root, 1),
            format!(r#"{{"runId":"run_theirs","pid":{theirs},"startedAt":"then"}}"#),
        )
        .unwrap();
        let mut beside = ProjectLock::acquire(&root, "beside", "start").unwrap();
        assert_eq!(beside.copy(), 2);
        assert!(matches!(
            ProjectLock::acquire_alone(&root, "clean", "start"),
            Err(LifecycleError::ActiveRun { ref run_id, pid }) if run_id == "run_theirs" && pid == theirs
        ));
        beside.release().unwrap();

        // One whose process has gone left the file behind, and holds nothing.
        let mut gone = std::process::Command::new("true").spawn().unwrap();
        let dead = gone.id();
        gone.wait().unwrap();
        fs::write(
            owner_file(&root, 1),
            format!(r#"{{"runId":"run_gone","pid":{dead},"startedAt":"then"}}"#),
        )
        .unwrap();
        assert_eq!(
            ProjectLock::acquire(&root, "after", "start")
                .unwrap()
                .copy(),
            1
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_copy_past_the_first_goes_a_day_after_its_last_run() {
        let root = project();
        let container = root.join(".supercov/workspaces");
        for copy in [
            "workspace",
            "workspace-2",
            "workspace-3",
            "workspace-4",
            "source-2",
        ] {
            fs::create_dir_all(container.join(copy).join("project")).unwrap();
        }
        fs::write(
            container.join(".supercov-workspace-store"),
            b"Supercov instrumented workspace. Safe to delete.\n",
        )
        .unwrap();
        let locks = root.join(".supercov/locks");
        fs::create_dir_all(&locks).unwrap();
        let long_ago = SystemTime::now() - Duration::from_secs(3 * 24 * 60 * 60);
        // The second and the fourth were last used three days ago, the third
        // an hour ago, and the fourth is in use now.
        for (copy, used) in [
            (2, long_ago),
            (3, SystemTime::now() - Duration::from_secs(3600)),
            (4, long_ago),
        ] {
            let lock = File::create(locks.join(format!("{}.lock", copy_lock_name(copy)))).unwrap();
            lock.set_modified(used).unwrap();
        }
        let held = take_lock(&locks.join("active-4.lock"), false, || false)
            .unwrap()
            .unwrap();
        held.0.set_modified(long_ago).unwrap();

        recover_abandoned_runs(&root, "now").unwrap();
        let left = |copy: &str| container.join(copy).exists();
        assert!(left("workspace"), "the first is never idle");
        assert!(!left("workspace-2") && !left("source-2"));
        assert!(left("workspace-3"));
        assert!(left("workspace-4"), "a copy in use is not removed");
        drop(held);
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleanup_is_project_locked_and_clean_alone_removes_owned_caches() {
        let root = project();
        let container = root.join(".supercov/workspaces");
        fs::create_dir_all(container.join("workspace/project")).unwrap();
        fs::write(
            container.join(".supercov-workspace-store"),
            b"Supercov instrumented workspace. Safe to delete.\n",
        )
        .unwrap();
        fs::create_dir_all(root.join(".supercov/cache/legacy")).unwrap();
        let mut active = ProjectLock::acquire(&root, "active", "start").unwrap();
        assert!(matches!(
            clean_storage(
                &root,
                CleanupOptions {
                    keep: 0,
                    dry_run: false
                },
                "now"
            ),
            Err(LifecycleError::ActiveRun { .. })
        ));
        assert!(container.exists());
        active.release().unwrap();

        let cleaned = clean_storage(
            &root,
            CleanupOptions {
                keep: 0,
                dry_run: false,
            },
            "now",
        )
        .unwrap();
        assert!(cleaned.removed_build_cache);
        assert!(!container.exists());
        assert!(!root.join(".supercov/cache").exists());
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    // What a failed run measured is the only record of hours of test time, and
    // it lived in the work tree that failing removes. #40 lost a run to a phase
    // reported twice after two and three-quarter hours, with nothing left to
    // diagnose it from.
    #[test]
    fn evidence_a_failed_run_measured_outlives_the_work_tree() {
        let root = project();
        let work = root.join(".supercov/work/run_kept");
        let evidence = work.join("python/evidence");
        fs::create_dir_all(evidence.join("worker-1")).unwrap();
        fs::write(evidence.join("main.jsonl"), "what the run saw").unwrap();
        fs::write(evidence.join("worker-1/mcdc.json"), "{}").unwrap();

        let kept = keep_failed_evidence(&root, &evidence, "run_kept").expect("the evidence");
        remove_stored_tree_deferred(&root, &work).unwrap();
        sweep_trash(&root).unwrap();

        assert!(!evidence.exists(), "the work tree still went");
        assert_eq!(
            fs::read_to_string(kept.join("main.jsonl")).unwrap(),
            "what the run saw",
        );
        // Nested workers are evidence too: a run measured by several of them
        // cannot be diagnosed from one.
        assert_eq!(
            fs::read_to_string(kept.join("worker-1/mcdc.json")).unwrap(),
            "{}"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nothing_to_keep_leaves_no_directory_to_puzzle_over() {
        let root = project();
        // A run that failed before it measured anything: the frontend never
        // reached the point of writing evidence, or wrote none.
        let missing = root.join(".supercov/work/run_early/python/evidence");
        assert!(keep_failed_evidence(&root, &missing, "run_early").is_none());
        fs::create_dir_all(&missing).unwrap();
        assert!(keep_failed_evidence(&root, &missing, "run_early").is_none());
        assert!(
            !root.join(".supercov/failed-evidence/run_early").exists(),
            "an empty directory is worse than none: it reads as evidence",
        );
        fs::remove_dir_all(root).unwrap();
    }

    // The failure is the only thing the user reads. Keeping the evidence and
    // not saying where is the same as not keeping it.
    #[test]
    fn the_failure_says_where_the_evidence_was_kept() {
        let root = project();
        let evidence = root.join(".supercov/work/run_told/python/evidence");
        fs::create_dir_all(&evidence).unwrap();
        fs::write(evidence.join("main.jsonl"), "what the run saw").unwrap();

        let told = note_kept_evidence(&root, &evidence, "run_told", "boom".into());
        // Taken from the keeper rather than spelled out here: a path written as
        // one string renders every separator the same way, which is not what a
        // Windows path built join by join looks like, so asserting on the
        // spelling tested this test instead of the message. Keeping is
        // idempotent, so asking again names the directory already kept.
        let kept = keep_failed_evidence(&root, &evidence, "run_told").expect("the evidence");
        assert_eq!(kept, root.join(FAILED_EVIDENCE).join("run_told"));
        assert!(
            told.starts_with("boom\n"),
            "the failure itself came second: {told}"
        );
        assert!(told.contains(&kept.display().to_string()), "{told}");

        // Nothing kept, nothing to say: the failure that matters is not buried
        // under one about copying it.
        let nothing = root.join(".supercov/work/run_empty/python/evidence");
        assert_eq!(
            note_kept_evidence(&root, &nothing, "run_empty", "boom".into()),
            "boom",
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_trim_leaves_kept_evidence_alone() {
        let root = project();
        let kept = kept_evidence(&root, "run_trimmed");

        // `--keep N` is the user trimming history, not asking for a reset, and
        // a retention pass runs on its own: either taking the evidence would
        // undo the keeping within one run.
        clean_storage(
            &root,
            CleanupOptions {
                keep: 20,
                dry_run: false,
            },
            "now",
        )
        .unwrap();
        let retention = cleanup_storage_locked(
            &root,
            CleanupOptions {
                keep: 0,
                dry_run: false,
            },
            false,
        )
        .unwrap();
        sweep_trash(&root).unwrap();

        assert!(retention.removed_failed_evidence.is_empty());
        assert!(kept.join("main.jsonl").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn cleaning_everything_reclaims_kept_evidence_and_says_which() {
        let root = project();
        let kept = kept_evidence(&root, "run_cleaned");

        let preview = clean_storage(
            &root,
            CleanupOptions {
                keep: 0,
                dry_run: true,
            },
            "now",
        )
        .unwrap();
        assert_eq!(
            preview.removed_failed_evidence,
            vec!["run_cleaned".to_string()]
        );
        assert!(kept.join("main.jsonl").exists(), "a preview removed it");

        let result = clean_storage(
            &root,
            CleanupOptions {
                keep: 0,
                dry_run: false,
            },
            "now",
        )
        .unwrap();
        sweep_trash(&root).unwrap();

        // Removing it is irreversible and it is not a run, so the summary has
        // to name it rather than fold it into the run count.
        assert_eq!(
            result.removed_failed_evidence,
            vec!["run_cleaned".to_string()]
        );
        assert!(!kept.exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_file_or_a_name_too_long_on_the_way_to_a_write_refuses_it() {
        let root = project();
        fs::create_dir_all(root.join(".supercov")).unwrap();
        fs::write(root.join(".supercov/file"), "").unwrap();
        assert!(matches!(
            atomic_write(&root, &root.join(".supercov/file/x/state.json"), b"{}"),
            Err(LifecycleError::UnsafePath(path)) if path == root.join(".supercov/file")
        ));
        let long = "x".repeat(300);
        assert!(matches!(
            atomic_write(&root, &root.join(".supercov").join(&long).join("state.json"), b"{}"),
            Err(LifecycleError::Io { path, .. }) if path == root.join(".supercov").join(&long)
        ));
        // A name that fits, but not with the temporary's suffix.
        let near = "x".repeat(250);
        assert!(matches!(
            atomic_write(&root, &root.join(".supercov").join(&near), b"{}"),
            Err(LifecycleError::Io { path, .. }) if path.extension().is_some_and(|extension| extension == "tmp")
        ));
        assert!(!root.join(".supercov").join(&near).exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_write_onto_a_directory_fails_and_leaves_no_temporary() {
        let root = project();
        let directory = root.join(".supercov/work/run_dir/state.json");
        fs::create_dir_all(directory.join("inside")).unwrap();
        assert!(matches!(
            atomic_write(&root, &directory, b"{}"),
            Err(LifecycleError::Io { path, .. }) if path == directory
        ));
        let left = fs::read_dir(directory.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(left, ["state.json"]);
        // The same directory is no state to read.
        assert!(matches!(
            update_run_state(&root, "run_dir", RunStateStatus::Failed, "now", None),
            Err(LifecycleError::Io { .. })
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_stored_tree_under_a_file_is_an_error_not_already_gone() {
        let root = project();
        fs::create_dir_all(root.join(".supercov/runs")).unwrap();
        fs::write(root.join(".supercov/runs/file"), "").unwrap();
        // Windows answers a path below a file as not found: there it is a
        // tree already gone. Either way the file is left alone.
        let removed = remove_stored_tree_deferred(&root, &root.join(".supercov/runs/file/run_x"));
        if cfg!(unix) {
            assert!(matches!(removed, Err(LifecycleError::Io { .. })));
        } else {
            assert!(matches!(removed, Ok(None)), "{removed:?}");
        }
        assert!(root.join(".supercov/runs/file").is_file());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn waiting_for_the_trash_sweep_gives_up_only_past_its_patience() {
        let (release, wait) = std::sync::mpsc::channel::<()>();
        *BACKGROUND_SWEEP.lock().unwrap() = Some(std::thread::spawn(move || {
            let _ = wait.recv();
        }));
        assert!(!finish_background_sweep(Duration::ZERO));
        drop(release);
        *BACKGROUND_SWEEP.lock().unwrap() = Some(std::thread::spawn(|| {
            std::thread::sleep(Duration::from_millis(50));
        }));
        assert!(finish_background_sweep(Duration::from_secs(30)));
        assert!(finish_background_sweep(Duration::ZERO));
    }

    /// An owner file naming this process is not another run's: a run takes
    /// the copy, and cleaning is not refused for it.
    #[test]
    fn a_copy_this_process_is_named_beside_is_not_held_by_another_run() {
        let root = project();
        let locks = root.join(".supercov/locks");
        fs::create_dir_all(&locks).unwrap();
        let mine = LockOwner {
            run_id: "run_earlier".into(),
            pid: std::process::id(),
            started_at: "earlier".into(),
        };
        fs::write(
            locks.join(format!("{}.json", copy_lock_name(1))),
            serde_json::to_vec(&mine).unwrap(),
        )
        .unwrap();
        let mut alone = ProjectLock::acquire_alone(&root, "clean-1", "now").unwrap();
        alone.release().unwrap();
        let mut lock = ProjectLock::acquire(&root, "run_mine", "now").unwrap();
        assert_eq!(lock.copy(), 1);
        lock.release().unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_run_named_but_without_its_evidence_is_not_published() {
        let root = project();
        let run = root.join(".supercov/runs/run_bare");
        fs::create_dir_all(&run).unwrap();
        fs::write(run.join("run.json"), r#"{"id":"run_bare"}"#).unwrap();
        fs::create_dir_all(root.join(".supercov/work/run_bare")).unwrap();
        assert!(!finalize_published_run(&root, "run_bare").unwrap());
        assert!(root.join(".supercov/work/run_bare").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn evidence_that_cannot_all_be_copied_is_not_kept_in_part() {
        let root = project();
        let evidence = root.join(".supercov/work/run_link/python/evidence");
        fs::create_dir_all(&evidence).unwrap();
        fs::write(evidence.join("main.jsonl"), "what the run saw").unwrap();
        std::os::unix::fs::symlink(root.join("nowhere"), evidence.join("zz.jsonl")).unwrap();
        assert!(keep_failed_evidence(&root, &evidence, "run_link").is_none());
        assert!(!root.join(FAILED_EVIDENCE).join("run_link").exists());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_idle_copy_with_part_of_it_gone_is_removed_as_far_as_it_is_there() {
        let root = project();
        let container = root.join(".supercov/workspaces");
        fs::create_dir_all(container.join("workspace-2/project")).unwrap();
        fs::write(
            container.join(".supercov-workspace-store"),
            b"Supercov instrumented workspace. Safe to delete.\n",
        )
        .unwrap();
        let locks = root.join(".supercov/locks");
        fs::create_dir_all(&locks).unwrap();
        File::create(locks.join("active-2.lock"))
            .unwrap()
            .set_modified(SystemTime::now() - Duration::from_secs(3 * 24 * 60 * 60))
            .unwrap();
        recover_abandoned_runs(&root, "now").unwrap();
        assert!(!container.join("workspace-2").exists());
        sweep_trash(&root).unwrap();
        fs::remove_dir_all(root).unwrap();
    }

    /// Each file operation of `scenario` failing in turn, on a project
    /// `setup` makes along with whatever else the scenario needs.
    fn fail_each_file_operation<Extra>(
        setup: impl Fn() -> (PathBuf, Extra),
        scenario: impl Fn(&Path, &Extra) -> Result<(), LifecycleError>,
    ) -> usize {
        crate::fault::fail_each_call(
            "fs",
            setup,
            |(root, extra)| scenario(root, extra),
            |(root, _)| {
                sweep_trash(&root).unwrap();
                fs::remove_dir_all(root).unwrap();
            },
        )
    }

    #[test]
    fn publishing_survives_any_file_operation_failing() {
        let id = "2026-01-01T00-00-00-100Z";
        let operations = fail_each_file_operation(
            || {
                let root = project();
                let evidence = analysable_evidence(&root, id);
                (root, evidence)
            },
            |root, (evidence, metadata)| publish_run(root, metadata, evidence).map(drop),
        );
        assert!(operations > 5, "{operations}");
    }

    #[test]
    fn recovering_runs_survives_any_file_operation_failing() {
        let operations = fail_each_file_operation(
            || {
                let root = project();
                let dead = "2026-01-01T00-00-00-000Z";
                let published = "2026-01-02T00-00-00-000Z";
                for id in [dead, published] {
                    fs::create_dir_all(
                        root.join(".supercov/work")
                            .join(id)
                            .join(root.file_name().unwrap()),
                    )
                    .unwrap();
                    fs::create_dir_all(root.join(".supercov/evidence").join(id)).unwrap();
                    write_run_state(&root, &state(&root, id, RunStateStatus::Testing, u32::MAX))
                        .unwrap();
                }
                let (evidence, bytes) = evidence(&root);
                publish_run(&root, &metadata(published, bytes), &evidence).unwrap();
                (root, ())
            },
            |root, ()| recover_abandoned_runs(root, "recovered").map(drop),
        );
        assert!(operations > 5, "{operations}");
    }

    #[test]
    fn cleaning_storage_survives_any_file_operation_failing() {
        let operations = fail_each_file_operation(
            || {
                let root = project();
                let (evidence, bytes) = evidence(&root);
                for id in ["2026-01-01T00-00-00-000Z", "2026-01-02T00-00-00-000Z"] {
                    let copy = root.join(format!("{id}.gz"));
                    fs::copy(&evidence, &copy).unwrap();
                    publish_run(&root, &metadata(id, bytes), &copy).unwrap();
                }
                fs::create_dir_all(root.join(".supercov/evidence/2026-01-03T00-00-00-000Z"))
                    .unwrap();
                kept_evidence(&root, "2025-12-31T00-00-00-000Z");
                fs::create_dir_all(root.join(".supercov/cache/build")).unwrap();
                (root, ())
            },
            |root, ()| {
                clean_storage(
                    root,
                    CleanupOptions {
                        keep: 1,
                        dry_run: false,
                    },
                    "now",
                )
                .map(drop)
            },
        );
        assert!(operations > 5, "{operations}");
    }

    /// A run id that is no run's, a path outside the store or climbing out
    /// of it, and a rename or write at a root with no directory above it,
    /// are each refused.
    #[test]
    fn what_a_store_cannot_hold_is_refused() {
        let root = std::env::temp_dir();
        assert!(matches!(
            ProjectLock::acquire(&root, "not/a run", "now"),
            Err(LifecycleError::InvalidRunId(_))
        ));
        for (path, leaf) in [(Path::new("/elsewhere"), true), (&root.join("../b"), true)] {
            assert!(matches!(
                reject_linked_ancestors(&root, path, leaf),
                Err(LifecycleError::UnsafePath(_))
            ));
        }
        let root_dir = Path::new("/");
        assert!(matches!(
            atomic_rename(root_dir, &root.join("x")),
            Err(LifecycleError::UnsafePath(_))
        ));
        assert!(matches!(
            atomic_rename(&root.join("x"), root_dir),
            Err(LifecycleError::UnsafePath(_))
        ));
        assert!(matches!(
            atomic_write(&root, root_dir, b""),
            Err(LifecycleError::UnsafePath(_))
        ));
    }

    /// Keeping a failed run's evidence survives any file operation failing:
    /// it keeps what it can, or nothing and no empty directory.
    #[test]
    fn keeping_failed_evidence_survives_any_file_operation_failing() {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let fixture = || {
            let root = std::env::temp_dir().join(format!(
                "supercov-kept-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir_all(root.join("evidence/worker")).unwrap();
            fs::write(root.join("evidence/a.raw"), "a").unwrap();
            fs::write(root.join("evidence/worker/b.raw"), "b").unwrap();
            root
        };
        let steps = crate::fault::fail_each_call(
            "fs",
            fixture,
            |root| {
                if let Some(kept) = keep_failed_evidence(root, &root.join("evidence"), "run") {
                    assert!(kept.join("worker/b.raw").is_file());
                }
                Ok::<(), ()>(())
            },
            |root| {
                let _ = fs::remove_dir_all(root);
            },
        );
        assert!(steps > 4, "{steps}");
    }

    #[test]
    fn a_root_without_a_name_is_never_locked() {
        assert!(matches!(
            ProjectLock::acquire(Path::new("/"), "run", "now"),
            Err(LifecycleError::UnsafePath(_))
        ));
    }

    #[test]
    fn locking_and_removing_trees_survive_any_file_operation_failing() {
        let locked = fail_each_file_operation(
            || (project(), ()),
            |root, ()| ProjectLock::acquire(root, "run", "now")?.release(),
        );
        let removed = fail_each_file_operation(
            || {
                let root = project();
                fs::create_dir_all(root.join(".supercov/work/run/deep")).unwrap();
                fs::write(root.join(".supercov/work/run/deep/file"), "x").unwrap();
                (root, ())
            },
            |root, ()| {
                remove_stored_tree_deferred(root, &root.join(".supercov/work/run"))?;
                sweep_trash(root).map(drop)
            },
        );
        assert!(locked > 2 && removed > 2, "{locked} {removed}");
    }

    #[test]
    fn each_lifecycle_error_says_what_failed() {
        let path = PathBuf::from(".supercov/runs");
        let messages = [
            LifecycleError::Io {
                path: path.clone(),
                source: io::Error::other("disk full"),
            },
            LifecycleError::InvalidRunId("../x".into()),
            LifecycleError::UnsafePath(path.clone()),
            LifecycleError::InvalidState("missing".into()),
            LifecycleError::ActiveRun {
                run_id: "run".into(),
                pid: 7,
            },
            LifecycleError::StartingRun,
            LifecycleError::Cleaning,
            LifecycleError::LockUnavailable,
            LifecycleError::PublicationExists("run".into()),
            LifecycleError::Metadata(serde_json::from_str::<u8>("x").unwrap_err()),
            LifecycleError::EvidenceLength {
                expected: 2,
                actual: 3,
            },
            LifecycleError::EvidenceChanged,
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                ".supercov/runs: disk full",
                "invalid coverage run ID: ../x",
                "unsafe Supercov storage path: .supercov/runs",
                "invalid run state: missing",
                "coverage run run is active in this project (pid 7). Its storage can be cleaned when no run is active.",
                "a coverage run is active in this project, starting. Its storage can be cleaned when no run is active.",
                "this project's Supercov storage is being cleaned. No test ran: start the run again when the cleaning has finished.",
                "could not acquire the Supercov project lock",
                "coverage run already exists: run",
                "invalid run metadata: expected value at line 1 column 1",
                "evidence length changed before publication: expected 2, got 3",
                "evidence changed during publication",
            ]
        );
    }
}
