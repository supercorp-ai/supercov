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

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::javascript_frontend::{authored_path, rewritten_files};

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

/// The launcher that stands in for `tool` in the workspace's `.bin`.
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

/// PATH for the test command with launchers first for the source tools it
/// has: a package manager's script finds a project's own tool in the
/// workspace's `.bin`, but `supercov -- biome check`, and a tool installed
/// for the whole machine, are found here. A name that is not on PATH gets no
/// launcher, so a script that asks whether a tool is installed hears what it
/// heard.
#[cfg(unix)]
pub fn path_with_launchers(workspace: &Path) -> Option<OsString> {
    use std::os::unix::fs::PermissionsExt;
    let path = std::env::var_os("PATH")?;
    let binary = std::env::current_exe().ok()?;
    let directory = workspace.join(".supercov/node_modules/.tools");
    remove(&directory).ok()?;
    let mut found = false;
    for name in source_tools() {
        let Some(tool) = std::env::split_paths(&path)
            .map(|directory| directory.join(&name))
            .find(|candidate| {
                fs::metadata(candidate).is_ok_and(|metadata| {
                    metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
                })
            })
        else {
            continue;
        };
        let link = directory.join(&name);
        fs::create_dir_all(&directory).ok()?;
        fs::write(&link, launcher(&binary, &tool)).ok()?;
        fs::set_permissions(&link, fs::Permissions::from_mode(0o755)).ok()?;
        found = true;
    }
    if !found {
        return None;
    }
    std::env::join_paths(std::iter::once(directory).chain(std::env::split_paths(&path))).ok()
}

#[cfg(not(unix))]
pub fn path_with_launchers(_workspace: &Path) -> Option<OsString> {
    None
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

#[cfg(not(unix))]
fn same_file(_left: &fs::Metadata, _right: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn link_directory(target: &Path, link: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn link_directory(_target: &Path, _link: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "a source view needs symbolic links",
    ))
}

fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
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
    link_directory(target, link)
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
            let authored = authored_path(workspace, &path);
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

/// What the tool left in the view that the workspace does not have.
#[derive(Default)]
struct Outcome {
    /// Rewritten files whose authored text the tool changed.
    changed_sources: Vec<PathBuf>,
}

/// Give the workspace what the tool wrote in its view: new files, files it
/// replaced, and the removal of files it deleted. A file it changed in place
/// is the workspace's own file already.
///
/// The authored text of a rewritten file is the exception. The workspace
/// holds what was instrumented from it, so a formatter's or a fixer's change
/// has nowhere to go during a measured run, and is reported instead.
fn reconcile(workspace: &Path, view: &Path, state: &View) -> io::Result<Outcome> {
    let mut outcome = Outcome::default();
    for path in &state.linked {
        if fs::symlink_metadata(view.join(path)).is_err() {
            remove(&workspace.join(path))?;
        }
    }
    for path in &state.authored {
        let unchanged = fs::read(view.join(path))
            .and_then(|current| Ok(current == fs::read(authored_path(workspace, path))?))
            .unwrap_or(false);
        if !unchanged {
            outcome.changed_sources.push(path.clone());
        }
    }
    reconcile_directory(workspace, view, Path::new(""), state)?;
    Ok(outcome)
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
            link_directory(&target, &to)?;
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
    let (Ok(workspace), Ok(directory)) = (
        fs::canonicalize(&named),
        std::env::current_dir().and_then(fs::canonicalize),
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
        if matches!(&*name_text, "PWD" | "OLDPWD" | "INIT_CWD" | "PATH")
            || name_text.starts_with("npm_")
        {
            let translated = in_view(&value, &names, &view);
            if translated != value {
                command.env(name, translated);
            }
        }
    }
    let code = status(&mut command);
    match reconcile(&workspace, &view, &state) {
        Ok(outcome) if !outcome.changed_sources.is_empty() => {
            let listed = outcome
                .changed_sources
                .iter()
                .take(3)
                .map(|path| path.display().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            eprintln!(
                "[supercov] {} changed {} source file(s) ({listed}{}); a measured run does not apply a tool's changes to source, so run it without Supercov to keep them",
                tool.file_name().unwrap_or_default().to_string_lossy(),
                outcome.changed_sources.len(),
                if outcome.changed_sources.len() > 3 {
                    ", …"
                } else {
                    ""
                },
            );
        }
        Ok(_) => {}
        Err(error) => eprintln!(
            "[supercov] what {} wrote could not be brought into the workspace: {error}",
            tool.display()
        ),
    }
    code
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
        let outcome = reconcile(&workspace, &view, &state).unwrap();
        assert_eq!(
            fs::read_to_string(workspace.join("reports/lint.json")).unwrap(),
            "[]"
        );
        assert_eq!(
            fs::read_to_string(workspace.join("README.md")).unwrap(),
            "replaced\n"
        );
        assert!(outcome.changed_sources.is_empty());
        assert!(!workspace.join(".git").exists());
        // The instrumented file stays what was instrumented.
        assert_eq!(
            fs::read_to_string(workspace.join("src/a.js")).unwrap(),
            "probe(); const a = 1;\n"
        );

        // A change to authored text has nowhere to go, and is named.
        fs::write(view.join("src/a.js"), "const a = 1\n").unwrap();
        let outcome = reconcile(&workspace, &view, &state).unwrap();
        assert_eq!(outcome.changed_sources, [PathBuf::from("src/a.js")]);
        assert_eq!(
            fs::read_to_string(workspace.join(".supercov/node_modules/.authored/src/a.js"))
                .unwrap(),
            "const a = 1;\n"
        );

        // The next tool starts from the workspace again.
        fs::remove_file(workspace.join("reports/lint.json")).unwrap();
        let state = refresh(&workspace, &view, &dependencies).unwrap();
        assert!(!view.join("reports/lint.json").exists());
        assert_eq!(
            fs::read_to_string(view.join("src/a.js")).unwrap(),
            "const a = 1;\n"
        );
        fs::remove_file(view.join("README.md")).unwrap();
        reconcile(&workspace, &view, &state).unwrap();
        assert!(!workspace.join("README.md").exists());
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
    fn a_path_in_an_argument_is_the_views() {
        let translated = in_view(
            OsStr::new("--config=/w/workspaces/workspace/p/biome.json"),
            &[Path::new("/w/workspaces/workspace/p")],
            Path::new("/w/workspaces/source/p"),
        );
        assert_eq!(translated, "--config=/w/workspaces/source/p/biome.json");
    }
}
