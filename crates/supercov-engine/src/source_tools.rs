//! Tools that judge the project's source as text.
//!
//! A linter, a formatter or a spell checker that a test script runs reads the
//! files of the instrumented workspace, and fails on code nobody wrote:
//! Biome on probe calls it would format otherwise, cspell on Supercov's own
//! names, knip on the runtime the files now import. ESLint and Prettier are
//! Node programs, and the preload hands them the authored text of each file
//! they read. A native program cannot be handed anything.
//!
//! Such a tool is started through a launcher in the workspace's own
//! `node_modules/.bin`, which calls back into this binary. It then runs in a
//! view of the workspace that holds every file as its author wrote it: a
//! directory beside the workspace with a hard link to each file the run did
//! not rewrite, and the authored text of each one it did. What the tool
//! writes there is the workspace's too, as if it had run in it.
//!
//! A tool that changes a source file, a formatter with `--write` or a linter
//! with `--fix`, changes the authored text. The workspace holds what was
//! instrumented from the text before, and written over that file the change
//! would leave it unmeasured: Prettier did exactly that through the preload,
//! and a file whose tests passed read 0% covered. The new text is kept
//! beside the instrumented copy instead. Tools that run later in the command
//! read it, the tests run the copy that is measured, and when the command
//! ends the change is the project's, as the command meant it to be.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::javascript_frontend::{authored_path, changed_directory, changed_path, rewritten_files};

/// The hidden command a launcher runs: `supercov __source-tool <tool> <args>`.
pub const SOURCE_TOOL_COMMAND: &str = "__source-tool";

/// Names added to [`SOURCE_TOOLS`], separated by commas.
pub const SOURCE_TOOLS_VARIABLE: &str = "SUPERCOV_SOURCE_TOOLS";

/// The tools measured to fail on an instrumented workspace and to pass in its
/// source view. Anything else that reads source as text can be named in
/// `SUPERCOV_SOURCE_TOOLS`.
pub const SOURCE_TOOLS: &[&str] = &["biome", "cspell", "dprint", "knip", "oxlint"];

/// The names a launcher is written for.
pub fn source_tools() -> BTreeSet<String> {
    let mut tools = SOURCE_TOOLS
        .iter()
        .map(|tool| (*tool).to_owned())
        .collect::<BTreeSet<_>>();
    if let Some(named) = std::env::var_os(SOURCE_TOOLS_VARIABLE) {
        tools.extend(
            named
                .to_string_lossy()
                .split(',')
                .map(str::trim)
                .filter(|name| !name.is_empty() && !name.contains(['/', '\\']))
                .map(str::to_owned),
        );
    }
    tools
}

/// The launcher that stands in for `tool` where a shell starts it.
pub fn launcher(binary: &Path, tool: &Path) -> String {
    let quoted = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"));
    format!(
        "#!/bin/sh\n# Runs {name} on the project's source as it was written; see `supercov --help`.\nexec {binary} {SOURCE_TOOL_COMMAND} {tool} \"$@\"\n",
        name = tool
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .replace('\n', " "),
        binary = quoted(binary),
        tool = quoted(tool),
    )
}

/// The same for `cmd.exe`, which is what npm runs a script with on Windows.
fn command_launcher(binary: &Path, tool: &Path) -> String {
    format!(
        "@ECHO off\r\n\"{}\" {SOURCE_TOOL_COMMAND} \"{}\" %*\r\n",
        binary.display(),
        tool.display()
    )
}

/// The same for PowerShell.
fn powershell_launcher(binary: &Path, tool: &Path) -> String {
    let quoted = |path: &Path| format!("'{}'", path.to_string_lossy().replace('\'', "''"));
    format!(
        "& {} {SOURCE_TOOL_COMMAND} {} @args\r\nexit $LASTEXITCODE\r\n",
        quoted(binary),
        quoted(tool)
    )
}

/// What stands in a workspace's `.bin` for the file `name` of the project's
/// `.bin` at `tools`, when that file starts a tool that judges source.
///
/// On Windows a tool is three files: the shim `cmd.exe` runs, one for
/// PowerShell and one for a POSIX shell. Each gets a launcher in its own
/// language, and all three run the `.cmd`.
pub fn launcher_for(
    judges: &BTreeSet<String>,
    binary: &Path,
    tools: &Path,
    name: &OsStr,
) -> Option<String> {
    launcher_on(cfg!(windows), judges, binary, tools, name)
}

fn launcher_on(
    windows: bool,
    judges: &BTreeSet<String>,
    binary: &Path,
    tools: &Path,
    name: &OsStr,
) -> Option<String> {
    let name = name.to_str()?;
    if windows {
        let (tool, kind) = match name.rsplit_once('.') {
            Some((tool, extension)) if extension.eq_ignore_ascii_case("cmd") => (tool, 1),
            Some((tool, extension)) if extension.eq_ignore_ascii_case("ps1") => (tool, 2),
            _ => (name, 0),
        };
        if !judges.contains(tool) {
            return None;
        }
        let shim = tools.join(format!("{tool}.cmd"));
        return shim.is_file().then(|| match kind {
            1 => command_launcher(binary, &shim),
            2 => powershell_launcher(binary, &shim),
            _ => launcher(binary, &shim),
        });
    }
    judges
        .contains(name)
        .then(|| launcher(binary, &tools.join(name)))
}

/// This binary, by a path a shell and `cmd.exe` can both be given.
pub fn launching_binary() -> Option<PathBuf> {
    std::env::current_exe()
        .ok()
        .map(crate::workspace::simplified)
}

/// Write a launcher where `path` is, replacing what is there. Never through
/// it: in a workspace whose dependencies are hard links, the file there is
/// the project's own.
pub fn write_launcher(path: &Path, text: &str) -> io::Result<()> {
    remove(path)?;
    fs::write(path, text)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    Ok(())
}

/// Put launchers in a `.bin` the workspace already has as a directory of its
/// own, which is how a workspace package's dependencies are laid out: for
/// each source tool the project's `.bin` at `tools` has, in place of the
/// entry the copy at `to` has for it.
pub fn place_launchers(tools: &Path, to: &Path) -> io::Result<()> {
    let judges = source_tools();
    let (Some(binary), Ok(entries)) = (launching_binary(), fs::read_dir(tools)) else {
        return Ok(());
    };
    if !fs::symlink_metadata(to).is_ok_and(|metadata| metadata.file_type().is_dir()) {
        return Ok(());
    }
    for entry in entries {
        let name = entry?.file_name();
        if let Some(text) = launcher_for(&judges, &binary, tools, &name) {
            write_launcher(&to.join(&name), &text)?;
        }
    }
    Ok(())
}

/// PATH for the test command with launchers first for the source tools it
/// has: a package manager's script finds a project's own tool in the
/// workspace's `.bin`, but `supercov -- biome check`, and a tool installed
/// for the whole machine, are found here. A name that is not on PATH gets no
/// launcher, so a script that asks whether a tool is installed hears what it
/// heard.
pub fn path_with_launchers(workspace: &Path) -> Option<OsString> {
    let path = std::env::var_os("PATH")?;
    let binary = launching_binary()?;
    let directory = workspace.join(".supercov/node_modules/.tools");
    remove(&directory).ok()?;
    // The forms a tool is installed in, in the order the system tries them,
    // and the launcher that answers to the name.
    let forms: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let mut found = false;
    for name in source_tools() {
        let Some(tool) = std::env::split_paths(&path)
            .flat_map(|directory| {
                forms
                    .iter()
                    .map(|form| directory.join(format!("{name}{form}")))
                    .collect::<Vec<_>>()
            })
            .find(|candidate| executable(candidate))
        else {
            continue;
        };
        fs::create_dir_all(&directory).ok()?;
        if cfg!(windows) {
            let launcher = command_launcher(&binary, &tool);
            write_launcher(&directory.join(format!("{name}.cmd")), &launcher).ok()?;
        } else {
            write_launcher(&directory.join(&name), &launcher(&binary, &tool)).ok()?;
        }
        found = true;
    }
    if !found {
        return None;
    }
    std::env::join_paths(std::iter::once(directory).chain(std::env::split_paths(&path))).ok()
}

#[cfg(unix)]
fn executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .is_ok_and(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(path: &Path) -> bool {
    path.is_file()
}

/// Where the source view of a workspace is kept: beside the directory that
/// holds the workspaces, under the same name.
pub fn source_view_path(workspace: &Path) -> Option<PathBuf> {
    let name = workspace.file_name()?;
    let kind = workspace.parent()?;
    let container = kind.parent()?;
    (kind.file_name()? == "workspace").then(|| container.join("source").join(name))
}

/// A view is kept under the project's `.supercov`, which the project's
/// `.gitignore` names, and a tool that honours ignore files, oxlint or
/// dprint, then finds no file in it. Such a tool stops reading the ignore
/// files of parent directories at the first directory that has a `.git`, so
/// a view whose workspace has none gets an empty one. To git itself an empty
/// `.git` is no repository, and it looks further up, as it did.
const BOUNDARY: &str = ".git";

/// A rewritten file's text as the command has it now: what one of its tools
/// made of it, or what its author wrote.
fn current_source(workspace: &Path, file: &Path) -> PathBuf {
    let changed = changed_path(workspace, file);
    if changed.is_file() {
        changed
    } else {
        authored_path(workspace, file)
    }
}

/// What a file of the view is, to tell afterwards what the tool did to it.
#[derive(Default)]
struct View {
    /// Files linked from the workspace, by their path in it.
    linked: BTreeSet<PathBuf>,
    /// Files that hold authored text.
    authored: BTreeSet<PathBuf>,
}

#[cfg(unix)]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    left.dev() == right.dev() && left.ino() == right.ino()
}

/// Where a file's identity is not to be had, two hard links to one file are
/// told by what they share: its length and the moment it was last written.
#[cfg(not(unix))]
fn same_file(left: &fs::Metadata, right: &fs::Metadata) -> bool {
    left.len() == right.len()
        && left.modified().is_ok()
        && left.modified().ok() == right.modified().ok()
}

/// A link at `link` to `target`: a symbolic link, or on Windows a junction
/// for a directory, which needs no privilege.
fn link_to(target: &Path, link: &Path) -> io::Result<()> {
    crate::workspace::create_link(target, link, target.is_dir())
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path),
        // A link to a directory is removed as a directory on Windows.
        Ok(_) => fs::remove_file(path).or_else(|_| fs::remove_dir(path)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Make `link` a symbolic link to `target`, whatever it was.
fn point(link: &Path, target: &Path) -> io::Result<()> {
    if fs::read_link(link).is_ok_and(|current| current == target) {
        return Ok(());
    }
    remove(link)?;
    link_to(target, link)
}

/// Make the file at `to` the file at `from`.
fn link_file(from: &Path, to: &Path) -> io::Result<()> {
    let source = fs::metadata(from)?;
    if fs::symlink_metadata(to).is_ok_and(|current| same_file(&current, &source)) {
        return Ok(());
    }
    remove(to)?;
    fs::hard_link(from, to).or_else(|_| fs::copy(from, to).map(|_| ()))
}

/// Give the file at `to` the contents of `from`, as a file of its own.
fn copy_file(from: &Path, to: &Path) -> io::Result<()> {
    let contents = fs::read(from)?;
    if fs::symlink_metadata(to).is_ok_and(|current| current.is_file())
        && fs::read(to).is_ok_and(|current| current == contents)
    {
        return Ok(());
    }
    remove(to)?;
    fs::write(to, contents)
}

/// Bring the view of `workspace` at `view` up to date: every file and
/// directory of the workspace, a rewritten file as its author wrote it, and
/// nothing else. Dependencies are the project's own, so a tool the judged one
/// starts in turn is the real one.
fn refresh(workspace: &Path, view: &Path, dependencies: &Path) -> io::Result<View> {
    let rewritten = rewritten_files(workspace)
        .into_iter()
        .map(PathBuf::from)
        .collect::<BTreeSet<_>>();
    let mut state = View::default();
    fs::create_dir_all(view)?;
    refresh_directory(
        workspace,
        view,
        dependencies,
        Path::new(""),
        &rewritten,
        &mut state,
    )?;
    Ok(state)
}

fn refresh_directory(
    workspace: &Path,
    view: &Path,
    dependencies: &Path,
    local: &Path,
    rewritten: &BTreeSet<PathBuf>,
    state: &mut View,
) -> io::Result<()> {
    let mut present = BTreeSet::new();
    for entry in fs::read_dir(workspace.join(local))? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".supercov" {
            continue;
        }
        let path = local.join(&name);
        let (from, to) = (workspace.join(&path), view.join(&path));
        let file_type = entry.file_type()?;
        present.insert(name.clone());
        if name == "node_modules" {
            let target = if local.as_os_str().is_empty() && dependencies.is_dir() {
                dependencies.to_path_buf()
            } else {
                from
            };
            point(&to, &target)?;
        } else if file_type.is_symlink() {
            point(&to, &fs::read_link(&from)?)?;
        } else if file_type.is_dir() {
            if !fs::symlink_metadata(&to).is_ok_and(|current| current.file_type().is_dir()) {
                remove(&to)?;
                fs::create_dir(&to)?;
            }
            refresh_directory(workspace, view, dependencies, &path, rewritten, state)?;
        } else if file_type.is_file() {
            let authored = current_source(workspace, &path);
            if rewritten.contains(&path) && authored.is_file() {
                copy_file(&authored, &to)?;
                state.authored.insert(path);
            } else {
                link_file(&from, &to)?;
                state.linked.insert(path);
            }
        }
    }
    let boundary = local.as_os_str().is_empty() && !present.contains(OsStr::new(BOUNDARY));
    if boundary && !view.join(BOUNDARY).is_dir() {
        remove(&view.join(BOUNDARY))?;
        fs::create_dir(view.join(BOUNDARY))?;
    }
    for entry in fs::read_dir(view.join(local))? {
        let entry = entry?;
        let kept =
            present.contains(&entry.file_name()) || (boundary && entry.file_name() == BOUNDARY);
        if !kept {
            remove(&entry.path())?;
        }
    }
    Ok(())
}

/// Give the workspace what the tool wrote in its view: new files, files it
/// replaced, and the removal of files it deleted. A file it changed in place
/// is the workspace's own file already.
///
/// The text of a rewritten file is the exception: the workspace holds what
/// was instrumented from it. A change to it is kept beside that copy, for
/// the tools that run next and for the project when the command ends.
/// Returns how many files the tool changed that way.
fn reconcile(workspace: &Path, view: &Path, state: &View) -> io::Result<usize> {
    let mut changed = 0;
    for path in &state.linked {
        if fs::symlink_metadata(view.join(path)).is_err() {
            remove(&workspace.join(path))?;
        }
    }
    for path in &state.authored {
        let Ok(text) = fs::read(view.join(path)) else {
            continue;
        };
        if fs::read(current_source(workspace, path)).is_ok_and(|current| current == text) {
            continue;
        }
        let kept = changed_path(workspace, path);
        if let Some(parent) = kept.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(&kept, text)?;
        changed += 1;
    }
    reconcile_directory(workspace, view, Path::new(""), state)?;
    Ok(changed)
}

fn reconcile_directory(
    workspace: &Path,
    view: &Path,
    local: &Path,
    state: &View,
) -> io::Result<()> {
    for entry in fs::read_dir(view.join(local))? {
        let entry = entry?;
        let path = local.join(entry.file_name());
        let (from, to) = (view.join(&path), workspace.join(&path));
        let file_type = entry.file_type()?;
        if path == Path::new(BOUNDARY) && fs::symlink_metadata(&to).is_err() {
            continue;
        }
        if file_type.is_dir() {
            if fs::symlink_metadata(&to).is_err() {
                fs::create_dir(&to)?;
            }
            reconcile_directory(workspace, view, &path, state)?;
        } else if file_type.is_file() && !state.authored.contains(&path) {
            link_file(&from, &to)?;
        } else if file_type.is_symlink()
            && fs::symlink_metadata(&to).is_err()
            && let Ok(target) = fs::read_link(&from)
        {
            link_to(&target, &to)?;
        }
    }
    Ok(())
}

/// `value` with every mention of the workspace's path replaced by the view's.
fn in_view(value: &OsStr, workspace: &[&Path], view: &Path) -> OsString {
    let Some(text) = value.to_str() else {
        return value.to_owned();
    };
    let mut translated = text.to_owned();
    for workspace in workspace {
        if let (Some(from), Some(to)) = (workspace.to_str(), view.to_str())
            && !from.is_empty()
        {
            translated = translated.replace(from, to);
        }
    }
    translated.into()
}

fn status(command: &mut Command) -> i32 {
    match command.status() {
        Ok(status) => status.code().unwrap_or(1),
        Err(error) => {
            eprintln!(
                "[supercov] could not start {}: {error}",
                command.get_program().to_string_lossy()
            );
            127
        }
    }
}

/// Run `tool` with `arguments` in the source view of the workspace this
/// process was started in, and return its exit status. Outside a workspace
/// of a run, the tool runs where it was started.
pub fn run_source_tool(tool: &Path, arguments: &[OsString]) -> i32 {
    let run_as_is = || status(Command::new(tool).args(arguments));
    let Some(named) = std::env::var_os("SUPERCOV_PROJECT_ROOT").map(PathBuf::from) else {
        return run_as_is();
    };
    // By paths a child process can be started in: Windows' own canonical
    // form is one `cmd.exe` refuses as a working directory.
    let (Ok(workspace), Ok(directory)) = (
        crate::workspace::canonicalize_simplified(&named),
        std::env::current_dir().and_then(crate::workspace::canonicalize_simplified),
    ) else {
        return run_as_is();
    };
    let (Ok(local), Some(view)) = (
        directory.strip_prefix(&workspace),
        source_view_path(&workspace),
    ) else {
        return run_as_is();
    };
    let dependencies = std::env::var_os("SUPERCOV_SOURCE_PROJECT_ROOT")
        .map(|root| PathBuf::from(root).join("node_modules"))
        .unwrap_or_else(|| workspace.join("node_modules"));
    let state = match refresh(&workspace, &view, &dependencies) {
        Ok(state) => state,
        Err(error) => {
            eprintln!(
                "[supercov] {} reads the instrumented files: the project's source could not be laid out for it at {} ({error})",
                tool.display(),
                view.display()
            );
            return run_as_is();
        }
    };
    let names = [workspace.as_path(), named.as_path()];
    let mut command = Command::new(tool);
    command.current_dir(view.join(local)).args(
        arguments
            .iter()
            .map(|argument| in_view(argument, &names, &view)),
    );
    for (name, value) in std::env::vars_os() {
        // Where the tool is, and where a package manager says it is. PATH
        // too: the view's dependencies are the project's, so what the tool
        // starts in turn is the real one. Everything else that names the
        // workspace is Supercov's, the preload in NODE_OPTIONS included, and
        // belongs to the workspace still.
        let name_text = name.to_string_lossy();
        if ["PWD", "OLDPWD", "INIT_CWD", "PATH"]
            .iter()
            .any(|location| name_text.eq_ignore_ascii_case(location))
            || name_text.starts_with("npm_")
        {
            let translated = in_view(&value, &names, &view);
            if translated != value {
                command.env(name, translated);
            }
        }
    }
    let code = status(&mut command);
    if let Err(error) = reconcile(&workspace, &view, &state) {
        eprintln!(
            "[supercov] what {} wrote could not be brought into the workspace: {error}",
            tool.display()
        );
    }
    code
}

/// The source files the command's tools changed, by what became of each.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SourceChanges {
    /// Changed in the project, as the command meant.
    pub applied: Vec<PathBuf>,
    /// Left alone: the project's file is no longer the one the run started
    /// from, and the change was made to that one.
    pub kept_back: Vec<PathBuf>,
}

/// Forget the changes a command before this one left behind.
pub fn clear_source_changes(workspace: &Path) -> io::Result<()> {
    remove(&changed_directory(workspace))
}

/// Give the project the changes the command's tools made to its source
/// files. Each was made to the text the run started from, so it is applied
/// only where the project still has that text.
pub fn apply_source_changes(root: &Path, workspace: &Path) -> io::Result<SourceChanges> {
    let mut changes = SourceChanges::default();
    let directory = changed_directory(workspace);
    if directory.is_dir() {
        apply_directory(root, workspace, &directory, Path::new(""), &mut changes)?;
        remove(&directory)?;
    }
    changes.applied.sort();
    changes.kept_back.sort();
    Ok(changes)
}

fn apply_directory(
    root: &Path,
    workspace: &Path,
    directory: &Path,
    local: &Path,
    changes: &mut SourceChanges,
) -> io::Result<()> {
    for entry in fs::read_dir(directory.join(local))? {
        let entry = entry?;
        let path = local.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            apply_directory(root, workspace, directory, &path, changes)?;
            continue;
        }
        let text = fs::read(entry.path())?;
        let started_from = fs::read(authored_path(workspace, &path)).ok();
        // A tool that writes every file it formats, changed or not.
        if started_from.as_ref() == Some(&text) {
            continue;
        }
        let project = root.join(&path);
        if started_from.is_some() && fs::read(&project).ok() == started_from {
            // Written in place, as a formatter writes it: the file keeps its
            // mode and whatever links to it.
            fs::write(&project, text)?;
            changes.applied.push(path);
        } else {
            changes.kept_back.push(path);
        }
    }
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static UNIQUE: AtomicU64 = AtomicU64::new(0);

    /// A workspace with one rewritten file, one plain file, a dependency and
    /// Supercov's own directory.
    fn workspace() -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "supercov-source-view-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            UNIQUE.fetch_add(1, Ordering::Relaxed)
        ));
        let workspace = root.join("workspaces/workspace/project");
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir_all(workspace.join("node_modules/.bin")).unwrap();
        fs::create_dir_all(workspace.join(".supercov/node_modules/.authored/src")).unwrap();
        fs::write(workspace.join("src/a.js"), "probe(); const a = 1;\n").unwrap();
        fs::write(
            workspace.join(".supercov/node_modules/.authored/src/a.js"),
            "const a = 1;\n",
        )
        .unwrap();
        fs::write(
            workspace.join(".supercov/node_modules/authored-sources.json"),
            "[\"src/a.js\"]",
        )
        .unwrap();
        fs::write(workspace.join("README.md"), "read me\n").unwrap();
        let dependencies = root.join("project/node_modules");
        fs::create_dir_all(&dependencies).unwrap();
        let view = source_view_path(&workspace).unwrap();
        (workspace, view, dependencies)
    }

    #[test]
    fn a_view_holds_each_file_as_its_author_wrote_it() {
        let (workspace, view, dependencies) = workspace();
        let state = refresh(&workspace, &view, &dependencies).unwrap();
        assert_eq!(
            fs::read_to_string(view.join("src/a.js")).unwrap(),
            "const a = 1;\n"
        );
        assert!(same_file(
            &fs::metadata(view.join("README.md")).unwrap(),
            &fs::metadata(workspace.join("README.md")).unwrap()
        ));
        assert_eq!(
            fs::read_link(view.join("node_modules")).unwrap(),
            dependencies
        );
        assert!(!view.join(".supercov").exists());
        assert_eq!(state.authored.len(), 1);
        assert!(view.join(".git").is_dir());

        // What a tool writes there is the workspace's: a new file, a file
        // replaced by another, a file removed, a file changed in place.
        fs::create_dir(view.join("reports")).unwrap();
        fs::write(view.join("reports/lint.json"), "[]").unwrap();
        fs::remove_file(view.join("README.md")).unwrap();
        fs::write(view.join("README.md"), "replaced\n").unwrap();
        let changed = reconcile(&workspace, &view, &state).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("reports/lint.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("README.md")).unwrap(),
            "replaced\n"
        );
        assert_eq!(changed, 0);
        assert!(!workspace.join(".git").exists());
        // The instrumented file stays what was instrumented.
        assert_eq!(
            fs::read_to_string(workspace.join("src/a.js")).unwrap(),
            "probe(); const a = 1;\n"
        );

        // A change to a source file is kept beside the instrumented copy,
        // which stays what the tests run, and the authored text stays what
        // the run started from.
        fs::write(view.join("src/a.js"), "const a = 1\n").unwrap();
        assert_eq!(reconcile(&workspace, &view, &state).unwrap(), 1);
        assert_eq!(
            fs::read_to_string(workspace.join(".supercov/node_modules/.changed/src/a.js")).unwrap(),
            "const a = 1\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join(".supercov/node_modules/.authored/src/a.js"))
                .unwrap(),
            "const a = 1;\n"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("src/a.js")).unwrap(),
            "probe(); const a = 1;\n"
        );
        // Nothing more to keep when the next tool leaves it as it found it.
        assert_eq!(reconcile(&workspace, &view, &state).unwrap(), 0);

        // The next tool starts from the workspace again.
        fs::remove_file(workspace.join("reports/lint.json")).unwrap();
        let state = refresh(&workspace, &view, &dependencies).unwrap();
        assert!(!view.join("reports/lint.json").exists());
        // And from the source as the tool before it left it.
        assert_eq!(
            fs::read_to_string(view.join("src/a.js")).unwrap(),
            "const a = 1\n"
        );
        fs::remove_file(view.join("README.md")).unwrap();
        reconcile(&workspace, &view, &state).unwrap();
        assert!(!workspace.join("README.md").exists());
        fs::remove_dir_all(workspace.ancestors().nth(3).unwrap()).unwrap();
    }

    #[test]
    fn a_tools_change_to_source_is_the_projects_when_the_command_ends() {
        let (workspace, _, dependencies) = workspace();
        let root = dependencies.parent().unwrap().to_path_buf();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(workspace.join("lib")).unwrap();
        // Three rewritten files a formatter wrote. One of them was edited in
        // the project while the command ran, and one was written as it was.
        for (file, authored, project, written) in [
            (
                "src/a.js",
                "const a = 1;\n",
                "const a = 1;\n",
                "const a = 1\n",
            ),
            (
                "src/b.js",
                "const b = 2;\n",
                "const b = 3;\n",
                "const b = 2\n",
            ),
            (
                "src/c.js",
                "const c = 4\n",
                "const c = 4\n",
                "const c = 4\n",
            ),
        ] {
            fs::write(authored_path(&workspace, Path::new(file)), authored).unwrap();
            fs::write(root.join(file), project).unwrap();
            let changed = changed_path(&workspace, Path::new(file));
            fs::create_dir_all(changed.parent().unwrap()).unwrap();
            fs::write(changed, written).unwrap();
        }

        let changes = apply_source_changes(&root, &workspace).unwrap();

        assert_eq!(changes.applied, [PathBuf::from("src/a.js")]);
        assert_eq!(changes.kept_back, [PathBuf::from("src/b.js")]);
        assert_eq!(
            fs::read_to_string(root.join("src/a.js")).unwrap(),
            "const a = 1\n"
        );
        assert_eq!(
            fs::read_to_string(root.join("src/b.js")).unwrap(),
            "const b = 3;\n"
        );
        // Applied once: the next command starts with nothing to apply.
        assert_eq!(
            apply_source_changes(&root, &workspace).unwrap(),
            SourceChanges::default()
        );
        fs::remove_dir_all(workspace.ancestors().nth(3).unwrap()).unwrap();
    }

    #[test]
    fn a_launcher_survives_a_quote_in_a_path() {
        let text = launcher(
            Path::new("/opt/it's here/supercov"),
            Path::new("/work/app/node_modules/.bin/biome"),
        );
        assert!(text.starts_with("#!/bin/sh\n"), "{text}");
        assert!(
            text.contains("exec '/opt/it'\\''s here/supercov' __source-tool '/work/app/node_modules/.bin/biome' \"$@\"\n"),
            "{text}"
        );
    }

    #[test]
    fn a_windows_tool_gets_a_launcher_for_each_of_its_shims() {
        // npm writes three files for a tool on Windows: one `cmd.exe` runs,
        // one for PowerShell, one for a POSIX shell. Each is replaced in its
        // own language, and each runs the `.cmd`.
        let (workspace, ..) = workspace();
        let tools = workspace.join("node_modules/.bin");
        for file in ["biome", "biome.cmd", "biome.ps1", "jest.cmd"] {
            fs::write(tools.join(file), "").unwrap();
        }
        let judges = BTreeSet::from(["biome".to_owned()]);
        let binary = Path::new("C:/Program Files/supercov.exe");
        let shim = tools.join("biome.cmd");
        let launcher = |name: &str| launcher_on(true, &judges, binary, &tools, OsStr::new(name));
        assert_eq!(
            launcher("biome.cmd").unwrap(),
            format!(
                "@ECHO off\r\n\"C:/Program Files/supercov.exe\" __source-tool \"{}\" %*\r\n",
                shim.display()
            )
        );
        assert_eq!(
            launcher("biome.ps1").unwrap(),
            format!(
                "& 'C:/Program Files/supercov.exe' __source-tool '{}' @args\r\nexit $LASTEXITCODE\r\n",
                shim.display()
            )
        );
        assert!(launcher("biome").unwrap().starts_with("#!/bin/sh\n"));
        assert!(
            launcher("biome")
                .unwrap()
                .contains(&format!("'{}'", shim.display()))
        );
        // A tool that runs code is started as it is.
        assert_eq!(launcher("jest.cmd"), None);
        // Elsewhere a tool is one file, started by its name.
        assert!(launcher_on(false, &judges, binary, &tools, OsStr::new("biome")).is_some());
        assert_eq!(
            launcher_on(false, &judges, binary, &tools, OsStr::new("biome.cmd")),
            None
        );
        fs::remove_dir_all(workspace.ancestors().nth(3).unwrap()).unwrap();
    }

    #[test]
    fn a_package_bin_of_its_own_gets_launchers_in_place() {
        // A workspace package's dependencies are a tree of their own in the
        // workspace, hard links to the project's files where the filesystem
        // has no clones. A launcher replaces the entry: written through it,
        // it would have been written into the project.
        let (workspace, _, dependencies) = workspace();
        let tools = dependencies.join(".bin");
        fs::create_dir_all(&tools).unwrap();
        fs::write(tools.join("oxlint"), "the project's own\n").unwrap();
        fs::write(tools.join("jest"), "runs code\n").unwrap();
        let copy = workspace.join("packages/app/node_modules/.bin");
        fs::create_dir_all(&copy).unwrap();
        fs::hard_link(tools.join("oxlint"), copy.join("oxlint")).unwrap();
        fs::hard_link(tools.join("jest"), copy.join("jest")).unwrap();

        place_launchers(&tools, &copy).unwrap();

        assert!(
            fs::read_to_string(copy.join("oxlint"))
                .unwrap()
                .contains("__source-tool"),
        );
        assert_eq!(
            fs::read_to_string(tools.join("oxlint")).unwrap(),
            "the project's own\n"
        );
        assert_eq!(
            fs::read_to_string(copy.join("jest")).unwrap(),
            "runs code\n"
        );
        fs::remove_dir_all(workspace.ancestors().nth(3).unwrap()).unwrap();
    }

    #[test]
    fn a_path_in_an_argument_is_the_views() {
        let translated = in_view(
            OsStr::new("--config=/w/workspaces/workspace/p/biome.json"),
            &[Path::new("/w/workspaces/workspace/p")],
            Path::new("/w/workspaces/source/p"),
        );
        assert_eq!(translated, "--config=/w/workspaces/source/p/biome.json");
    }
}
