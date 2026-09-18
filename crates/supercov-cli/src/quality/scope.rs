//! Which files an assessment is about.
//!
//! This mirrors the coverage side's source scope rather than inventing a second
//! answer, because a reviewer reading `runs patch` and `quality patch` together
//! should be looking at the same set of files. The engine's own
//! `discover_source_scope` cannot be reused: it matches JavaScript extensions
//! only, and a quality assessment covers every language the CLI can read.
//!
//! Three decisions, in the engine's vocabulary:
//!
//! - **Included.** Under a source root that was discovered or declared.
//! - **Excluded.** Test code, generated output, or outside explicit roots, each
//!   carrying the reason it was left out.
//! - **Ambiguous.** First-party source under no recognised root. This is the
//!   state that matters: it is never silently dropped, it is reported, and
//!   `SUPERCOV_SOURCE_ROOTS` is how a project answers it.
//!
//! An allowlist, not a denylist. Everything is out until a root puts it in,
//! which is what stops a prototype tree or a vendored clone from being counted
//! as the product.

use serde::Serialize;
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

/// Directories that hold a package's own code, across the languages assessed.
/// `internal`, `pkg` and `cmd` are Go's; `src/main/java` is reached through
/// `src`.
const SOURCE_DIRECTORIES: &[&str] = &[
    "api",
    "app",
    "cmd",
    "client",
    "functions",
    "internal",
    "lib",
    "pkg",
    "server",
    "src",
];

/// Directories whose children are packages by convention, so a manifest found
/// inside one is a real package root rather than an unrelated checkout.
const PACKAGE_PARENTS: &[&str] = &["apps", "crates", "packages", "services", "workspaces"];

/// A file that declares a package, in any of the ecosystems assessed.
const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "Gemfile",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
    "go.mod",
    "package.json",
    "pom.xml",
    "pyproject.toml",
    "setup.py",
];

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Included,
    Excluded,
    Ambiguous,
}

#[derive(Clone, Debug, Serialize)]
pub struct Entry {
    pub path: String,
    pub status: Status,
    pub reason: String,
    pub package_root: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct Scope {
    /// `explicit` when SUPERCOV_SOURCE_ROOTS named the roots.
    pub mode: &'static str,
    pub roots: Vec<String>,
    pub entries: Vec<Entry>,
}

impl Scope {
    pub fn included(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.status == Status::Included)
    }

    pub fn ambiguous(&self) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.status == Status::Ambiguous)
            .count()
    }

    /// Counts by status and, for what was left out, by reason.
    pub fn summary(&self) -> Value {
        let mut reasons: BTreeMap<&str, usize> = BTreeMap::new();
        for entry in &self.entries {
            if entry.status != Status::Included {
                *reasons.entry(entry.reason.as_str()).or_default() += 1;
            }
        }
        json!({
            "mode": self.mode,
            "roots": self.roots,
            "included": self.included().count(),
            "ambiguous": self.ambiguous(),
            "excluded": self.entries.iter()
                .filter(|e| e.status == Status::Excluded).count(),
            "reasons": reasons,
        })
    }

    /// What a reader should be told when the scope is not confident.
    pub fn limitation(&self) -> Option<String> {
        let ambiguous = self.ambiguous();
        (ambiguous > 0).then(|| format!(
            "{ambiguous} source files sit under no recognised source root and were not assessed. \
             Read them with `supercov quality scope`, and declare your roots with \
             SUPERCOV_SOURCE_ROOTS=src,app if they are first-party code."
        ))
    }
}

fn manifest_in(directory: &Path) -> bool {
    MANIFESTS.iter().any(|name| directory.join(name).is_file())
}

/// Package directories a root manifest declares, so a member that lives
/// somewhere unconventional is still a package.
fn declared_members(root: &Path) -> BTreeSet<PathBuf> {
    let mut found = BTreeSet::new();
    let mut add = |pattern: &str| {
        // A trailing `*` is the only glob either ecosystem uses in practice.
        match pattern.strip_suffix("/*") {
            Some(parent) => {
                if let Ok(entries) = std::fs::read_dir(root.join(parent)) {
                    for entry in entries.flatten() {
                        if entry.path().is_dir() {
                            found.insert(entry.path());
                        }
                    }
                }
            }
            None => {
                let path = root.join(pattern);
                if path.is_dir() {
                    found.insert(path);
                }
            }
        }
    };
    if let Ok(text) = std::fs::read_to_string(root.join("package.json"))
        && let Ok(manifest) = serde_json::from_str::<Value>(&text)
    {
        let listed = manifest["workspaces"]
            .as_array()
            .cloned()
            .or_else(|| manifest["workspaces"]["packages"].as_array().cloned())
            .unwrap_or_default();
        for pattern in listed.iter().filter_map(|v| v.as_str()) {
            add(pattern);
        }
    }
    // Cargo's members are a TOML array of strings under [workspace]; reading
    // them by line avoids a TOML dependency for one field.
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) {
        let mut inside = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                inside = line == "[workspace]";
                continue;
            }
            if inside && line.starts_with("members") {
                inside = true;
            }
            if inside && let Some(start) = line.find('"') {
                let rest = &line[start + 1..];
                if let Some(end) = rest.find('"') {
                    add(&rest[..end]);
                }
            }
        }
    }
    found
}

/// Every directory that holds a package, by the same rule the coverage scope
/// uses: a manifest directly under the root or under a conventional package
/// parent, plus anything a root manifest declares.
fn package_roots(root: &Path) -> BTreeSet<PathBuf> {
    fn visit(root: &Path, directory: &Path, depth: usize, found: &mut BTreeSet<PathBuf>) {
        if depth > 5 {
            return;
        }
        let Ok(entries) = std::fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            if !path.is_dir() || name.starts_with('.') || super::ignored_directory(&name) {
                continue;
            }
            // A directory with its own checkout is somebody else's repository.
            if path.join(".git").exists() {
                continue;
            }
            let relative = path.strip_prefix(root).unwrap_or(&path);
            // A manifest inside a test tree belongs to a fixture, not to the
            // product. Without this, `tests/fixtures/.../packages/server/src`
            // becomes a source root because `packages` is a package parent.
            if super::skipped_path(&relative.to_string_lossy().replace('\\', "/")).is_some() {
                continue;
            }
            let under_parent = relative
                .components()
                .any(|c| PACKAGE_PARENTS.contains(&c.as_os_str().to_string_lossy().as_ref()));
            if manifest_in(&path) && (depth == 0 || under_parent) {
                found.insert(path.clone());
            }
            visit(root, &path, depth + 1, found);
        }
    }
    let mut found = BTreeSet::from([root.to_owned()]);
    visit(root, root, 0, &mut found);
    found.extend(declared_members(root));
    found
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Decide the scope for a set of already-discovered source files.
///
/// `files` are absolute paths the caller found; this classifies them and says
/// which roots it used. Configured roots switch the mode to explicit, where
/// anything outside them is excluded rather than left ambiguous.
pub fn classify(root: &Path, files: &[PathBuf], configured: Option<&[String]>) -> Scope {
    let explicit = configured.is_some_and(|roots| !roots.is_empty());
    let roots: BTreeSet<PathBuf> = if explicit {
        configured
            .unwrap_or_default()
            .iter()
            .map(|name| root.join(name.trim()))
            .filter(|path| path.exists())
            .collect()
    } else {
        let packages = package_roots(root);
        let mut roots = BTreeSet::new();
        for package in &packages {
            let candidates: Vec<PathBuf> = SOURCE_DIRECTORIES
                .iter()
                .map(|name| package.join(name))
                .filter(|path| path.is_dir())
                .collect();
            if candidates.is_empty() && package != root {
                // A declared package that keeps its code somewhere
                // unconventional is still first-party source, so the package
                // directory itself becomes the root.
                roots.insert(package.clone());
            } else {
                roots.extend(candidates);
            }
        }
        roots
    };

    let packages = if explicit {
        BTreeSet::new()
    } else {
        package_roots(root)
    };
    let nearest = |path: &Path| -> Option<String> {
        packages
            .iter()
            .filter(|package| path.starts_with(package) && *package != root)
            .max_by_key(|package| package.components().count())
            .map(|package| relative(root, package))
    };

    let mut entries = Vec::new();
    for path in files {
        let file = relative(root, path);
        let entry = |status, reason: &str| Entry {
            path: file.clone(),
            status,
            reason: reason.to_owned(),
            package_root: nearest(path),
        };
        if let Some(skipped) = super::skipped_path(&file) {
            entries.push(entry(Status::Excluded, skipped.reason()));
        } else if roots.iter().any(|dir| path.starts_with(dir)) {
            entries.push(entry(
                Status::Included,
                if explicit {
                    "explicit source root"
                } else {
                    "discovered package source root"
                },
            ));
        } else if explicit {
            entries.push(entry(Status::Excluded, "outside explicit source roots"));
        } else {
            entries.push(entry(Status::Ambiguous, "unclassified first-party source"));
        }
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Scope {
        mode: if explicit { "explicit" } else { "automatic" },
        roots: roots.iter().map(|path| relative(root, path)).collect(),
        entries,
    }
}

/// The roots a project declared, if any.
pub fn configured_roots() -> Option<Vec<String>> {
    let value = std::env::var("SUPERCOV_SOURCE_ROOTS").ok()?;
    let roots: Vec<String> = value
        .split(',')
        .map(|part| part.trim().to_owned())
        .filter(|part| !part.is_empty())
        .collect();
    (!roots.is_empty()).then_some(roots)
}
