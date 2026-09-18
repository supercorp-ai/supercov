//! Which change to review, and the two versions of every file in it.
//!
//! Three ranges cover the three moments a review is wanted: before a commit,
//! before a push, and on a pull request. They differ only in where `before` and
//! `after` are read from, so one selector serves all three and any later command
//! that works on a change can take the same flags.
//!
//! Whole files are read, never hunks. A hunk cannot answer whether a change
//! introduced a property or whether the file already had it, which is the whole
//! question, so the extra tokens are the point rather than an oversight.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where the two versions of each file come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Range {
    /// Working tree against the index: what is not yet staged.
    Unstaged,
    /// Index against HEAD: what a commit would contain.
    Staged,
    /// Working tree against a named commit, branch or tag.
    Since(String),
}

impl Range {
    pub fn describe(&self) -> String {
        match self {
            Range::Unstaged => "unstaged changes".into(),
            Range::Staged => "staged changes".into(),
            Range::Since(reference) => format!("changes since {reference}"),
        }
    }

    /// How the range names itself in a saved record, stable across runs.
    pub fn id(&self) -> String {
        match self {
            Range::Unstaged => "unstaged".into(),
            Range::Staged => "staged".into(),
            Range::Since(reference) => format!("since:{reference}"),
        }
    }
}

/// How a file was changed. A deletion cannot introduce anything, so it is not
/// reviewed; it is still reported so the count matches what Git says changed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Added,
    Modified,
    Deleted,
}

/// One changed file with both of its versions already read.
#[derive(Clone, Debug)]
pub struct Change {
    pub path: String,
    pub kind: Kind,
    /// Empty for an added file, which has no previous version.
    pub before: String,
    /// Empty for a deleted file.
    pub after: String,
    /// The unified diff for this path, used only when both versions together
    /// exceed the request budget.
    pub patch: String,
}

impl Change {
    pub fn reviewable(&self) -> bool {
        self.kind != Kind::Deleted && !self.after.is_empty()
    }
}

fn git(root: &Path, arguments: &[&str]) -> Result<Vec<u8>, String> {
    let output = Command::new("git")
        .current_dir(root)
        .args(arguments)
        .output()
        .map_err(|e| format!("cannot run git: {e}"))?;
    if !output.status.success() {
        let message = String::from_utf8_lossy(&output.stderr);
        let message = message.trim();
        return Err(if message.is_empty() {
            format!("git {} failed", arguments.join(" "))
        } else {
            format!("git {}: {message}", arguments.join(" "))
        });
    }
    Ok(output.stdout)
}

fn text(root: &Path, arguments: &[&str]) -> Result<String, String> {
    let bytes = git(root, arguments)?;
    String::from_utf8(bytes).map_err(|_| "git returned output that is not UTF-8".to_string())
}

/// A blob at a revision. The empty revision is the index, because `git show`
/// spells a staged blob `:path` with nothing before the colon.
///
/// Failures are reported, never swallowed. An earlier version returned an empty
/// string for any error, which turned a malformed revision spec into a silent
/// comparison against nothing: every property looked introduced and detection
/// quietly weakened instead of failing. A file that genuinely has no previous
/// version is an addition, and the caller already knows that from Git.
fn blob(root: &Path, revision: &str, path: &str) -> Result<String, String> {
    let spec = format!("{revision}:{path}");
    let bytes = git(root, &["show", &spec]).map_err(|e| format!("cannot read {spec}: {e}"))?;
    String::from_utf8(bytes).map_err(|_| format!("{spec} is not UTF-8"))
}

pub fn is_repository(root: &Path) -> bool {
    git(root, &["rev-parse", "--git-dir"]).is_ok()
}

/// The commit a range is measured against, for the record a review saves.
pub fn head(root: &Path) -> Option<String> {
    text(root, &["rev-parse", "HEAD"])
        .ok()
        .map(|s| s.trim().to_owned())
}

/// The arguments that select this range, shared by every git call that needs it.
fn selector(range: &Range) -> Vec<&str> {
    match range {
        Range::Unstaged => vec![],
        Range::Staged => vec!["--cached"],
        Range::Since(reference) => vec![reference.as_str()],
    }
}

fn patch(root: &Path, range: &Range, path: &str) -> String {
    let mut arguments = vec!["diff", "-U3"];
    arguments.extend(selector(range));
    arguments.extend(["--", path]);
    text(root, &arguments).unwrap_or_default()
}

fn name_status(root: &Path, range: &Range) -> Result<Vec<(Kind, String)>, String> {
    let mut arguments = vec!["diff", "--name-status", "--no-renames", "-z"];
    arguments.extend(selector(range));
    let raw = text(root, &arguments)?;
    // -z separates every field with NUL, so a path containing a newline or a
    // quote survives. Fields alternate status, path, status, path.
    let mut fields = raw.split('\0').filter(|s| !s.is_empty());
    let mut out = Vec::new();
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let kind = match status.chars().next() {
            Some('A') => Kind::Added,
            Some('D') => Kind::Deleted,
            _ => Kind::Modified,
        };
        out.push((kind, path.to_owned()));
    }
    Ok(out)
}

/// Every changed file in the range, with both versions read.
///
/// `only` restricts the review to paths the user named, matched as a prefix so
/// naming a directory reviews the changes inside it.
pub fn collect(root: &Path, range: &Range, only: &[PathBuf]) -> Result<Vec<Change>, String> {
    if !is_repository(root) {
        return Err("quality review needs a Git repository; run it inside one".into());
    }
    if let Range::Since(reference) = range {
        git(
            root,
            &["rev-parse", "--verify", &format!("{reference}^{{commit}}")],
        )
        .map_err(|_| format!("no commit named {reference}"))?;
    }
    let wanted = |path: &str| {
        only.is_empty()
            || only.iter().any(|p| {
                let p = p.to_string_lossy().trim_end_matches('/').to_owned();
                path == p || path.starts_with(&format!("{p}/"))
            })
    };
    let mut out = Vec::new();
    for (kind, path) in name_status(root, range)? {
        if !wanted(&path) {
            continue;
        }
        // The empty revision is the index: `git show :path`.
        let (before_rev, after_rev) = match range {
            Range::Unstaged => ("", None),
            Range::Staged => ("HEAD", Some("")),
            Range::Since(reference) => (reference.as_str(), None),
        };
        let before = if kind == Kind::Added {
            String::new()
        } else {
            blob(root, before_rev, &path)?
        };
        let after = match (kind, after_rev) {
            (Kind::Deleted, _) => String::new(),
            (_, Some(revision)) => blob(root, revision, &path)?,
            (_, None) => std::fs::read_to_string(root.join(&path)).unwrap_or_default(),
        };
        let patch = patch(root, range, &path);
        out.push(Change {
            path,
            kind,
            before,
            after,
            patch,
        });
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(out)
}
