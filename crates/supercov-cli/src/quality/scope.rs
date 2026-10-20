//! Which files an assessment is about.
//!
//! This mirrors the coverage side's source scope rather than inventing a second
//! answer, because a reviewer reading `runs patch` and `quality patch` together
//! should be looking at the same set of files. The engine's own
//! `discover_source_scope` answers for JavaScript and TypeScript only, and a
//! quality assessment covers every language the CLI can read, so the rules
//! below stand beside it. Where the engine does answer, its answer is taken: a
//! file a coverage run would measure is read here too (see `Measured`).
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
/// `cmd`, `internal` and `pkg` are Go's, `include` is where C and C++ keep a
/// public API, `sources` is SwiftPM's, and `src/main/java` is reached through
/// `src`. Matched without regard to case, because SwiftPM capitalises.
const SOURCE_DIRECTORIES: &[&str] = &[
    "api",
    "app",
    "client",
    "cmd",
    "functions",
    "include",
    "internal",
    "lib",
    "pkg",
    "server",
    "sources",
    "src",
];

/// Directories whose children are packages by convention, so a manifest found
/// inside one is a real package root rather than an unrelated checkout.
const PACKAGE_PARENTS: &[&str] = &["apps", "crates", "packages", "services", "workspaces"];

/// A file that declares a package, in any of the ecosystems assessed.
const MANIFESTS: &[&str] = &[
    "Cargo.toml",
    "Package.swift",
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
        let (files, sit, were, them, it) = if ambiguous == 1 {
            ("file", "sits", "was", "it", "it is")
        } else {
            ("files", "sit", "were", "them", "they are")
        };
        (ambiguous > 0).then(|| format!(
            "{ambiguous} source {files} {sit} under no recognised source root and {were} not assessed. \
             Read {them} with `supercov quality scope`, and declare your roots with \
             SUPERCOV_SOURCE_ROOTS=src,app if {it} first-party code."
        ))
    }
}

/// Locations a package's own manifest declares, resolved to files or
/// directories. Reading the manifest is how a project says where its code is
/// without anyone guessing: this repository's `package.json` names
/// `bin/supercov.js` and `./runtime/javascript/*.mjs`, neither of which is a
/// conventional source directory and both of which are shipped.
fn declared_entry_points(directory: &Path) -> Vec<PathBuf> {
    let mut targets: Vec<String> = Vec::new();
    fn strings(value: &Value, depth: usize, out: &mut Vec<String>) {
        if depth > 4 {
            return;
        }
        match value {
            Value::String(text) => out.push(text.clone()),
            Value::Array(items) => items.iter().for_each(|v| strings(v, depth + 1, out)),
            Value::Object(fields) => fields.values().for_each(|v| strings(v, depth + 1, out)),
            _ => {}
        }
    }
    if let Ok(text) = std::fs::read_to_string(directory.join("package.json"))
        && let Ok(manifest) = serde_json::from_str::<Value>(&text)
    {
        for key in ["main", "module", "browser", "bin", "exports"] {
            if let Some(value) = manifest.get(key) {
                strings(value, 0, &mut targets);
            }
        }
    }
    if let Ok(text) = std::fs::read_to_string(directory.join("Cargo.toml")) {
        for line in text.lines() {
            let line = line.trim();
            if let Some(rest) = line.strip_prefix("path")
                && let Some(value) = toml_strings(rest).next()
            {
                targets.push(value.to_owned());
            }
        }
        // Cargo compiles a `build.rs` beside the manifest without being told.
        if directory.join("build.rs").is_file() {
            targets.push("build.rs".to_owned());
        }
    }
    targets
        .into_iter()
        // A manifest names what it ships, which for a compiled package is the
        // build output. `"bin": "dist/index.js"` is a real declaration and a
        // useless source root: the code a reader would change is the input.
        .filter(|target| {
            !target
                .split('/')
                .any(|segment| super::ignored_directory(segment.trim_start_matches("./")))
        })
        .filter_map(|target| {
            // A subpath pattern such as `./dist/*.js` names the directory.
            let prefix = target.split('*').next()?.trim_end_matches('/');
            let prefix = prefix.strip_prefix("./").unwrap_or(prefix);
            if prefix.is_empty() || prefix.starts_with('/') || prefix.starts_with("..") {
                return None;
            }
            let path = directory.join(prefix);
            if path.is_dir() {
                return Some(path);
            }
            if !path.is_file() {
                return None;
            }
            // A declared file in a directory of its own, such as `bin`, makes
            // that directory a root so the files it loads travel with it. A
            // declared file sitting at the package root, such as Cargo's
            // `build.rs`, is only itself: taking its parent would swallow the
            // whole package.
            let parent = path.parent()?;
            Some(if parent == directory {
                path
            } else {
                parent.to_owned()
            })
        })
        .collect()
}

/// Manifests whose name varies, so the fixed list cannot hold them: Gradle lets
/// a module call its build file after itself, as JUnit's
/// `junit-jupiter-api.gradle.kts` does, and .NET names a project file after the
/// project. A project file also makes its directory a package at any depth,
/// because one project per directory is the convention and nesting is normal.
fn variable_manifest(directory: &Path) -> Option<bool> {
    let mut project_file = false;
    for entry in std::fs::read_dir(directory).ok()?.flatten() {
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        if name.ends_with(".gradle") || name.ends_with(".gradle.kts") {
            return Some(false);
        }
        if name.ends_with(".csproj") || name.ends_with(".fsproj") || name.ends_with(".vbproj") {
            project_file = true;
        }
    }
    project_file.then_some(true)
}

/// Whether a path names a file that declares a package, for the manifest list a
/// scope question carries as context.
pub fn declares_a_package(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = name.to_ascii_lowercase();
    MANIFESTS.contains(&name)
        || lower.ends_with(".gradle")
        || lower.ends_with(".gradle.kts")
        || lower.ends_with(".csproj")
        || lower.ends_with(".sln")
}

/// Every manifest of every package directory, relative to the root and
/// sorted: the context a scope question carries. Read from the packages
/// rather than from the source files found, which hold no manifest.
pub fn manifests(root: &Path) -> Vec<String> {
    let mut found: Vec<String> = package_roots(root)
        .iter()
        .filter_map(|directory| std::fs::read_dir(directory).ok())
        .flat_map(|entries| entries.flatten())
        .filter(|entry| entry.path().is_file())
        .map(|entry| relative(root, &entry.path()))
        .filter(|path| declares_a_package(path))
        .collect();
    found.sort();
    found
}

/// Whether a directory declares a package, and whether that declaration counts
/// at any depth.
fn manifest_in(directory: &Path) -> Option<bool> {
    if MANIFESTS.iter().any(|name| directory.join(name).is_file()) {
        return Some(false);
    }
    variable_manifest(directory)
}

/// The quoted strings on one line of TOML, basic (`"`) or literal (`'`).
fn toml_strings(line: &str) -> impl Iterator<Item = &str> {
    line.split(['"', '\'']).skip(1).step_by(2)
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
    // them by line avoids a TOML dependency for one field. Only `members`:
    // every quoted string of the table was taken, so a directory `exclude`
    // names became a package.
    if let Ok(text) = std::fs::read_to_string(root.join("Cargo.toml")) {
        let mut inside = false;
        let mut members = false;
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                inside = line == "[workspace]";
                members = false;
                continue;
            }
            if inside && line.starts_with("members") {
                members = true;
            }
            if members {
                toml_strings(line).for_each(&mut add);
                // The array closes on the line that holds its `]`.
                members = !line.contains(']');
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
            if !path.is_dir() || name.starts_with('.') || super::ignored_directory_at(&path) {
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
            match manifest_in(&path) {
                Some(true) => {
                    found.insert(path.clone());
                }
                Some(false) if depth == 0 || under_parent => {
                    found.insert(path.clone());
                }
                _ => {}
            }
            visit(root, &path, depth + 1, found);
        }
    }
    let mut found = BTreeSet::from([root.to_owned()]);
    visit(root, root, 0, &mut found);
    found.extend(declared_members(root));
    found
}

/// Code that is conventionally kept beside a package without being part of it.
///
/// A tool script is the coverage scope's own rule, in its words. Examples and
/// benchmarks are Cargo, Go and npm conventions for code that ships to nobody:
/// naming the reason is more useful than calling them unclassified, because
/// there is nothing for a project to declare.
fn beside_the_product(file: &str) -> Option<&'static str> {
    let lower = file.to_ascii_lowercase();
    let (directories, name) = match lower.rsplit_once('/') {
        Some((head, name)) => (head, name),
        None => ("", lower.as_str()),
    };
    for segment in directories.split('/') {
        match segment {
            "scripts" => return Some("tool script"),
            "examples" | "example" => return Some("example"),
            "benches" | "benchmarks" => return Some("benchmark"),
            // Code inside documentation is there to be read, not shipped.
            "docs" | "doc" | "documentation" => return Some("documentation"),
            _ => {}
        }
    }
    // Vitest keeps a benchmark beside the code it measures, as `*.bench.ts`.
    if name
        .split('.')
        .skip(1)
        .any(|part| matches!(part, "bench" | "benchmark"))
    {
        return Some("benchmark");
    }
    // How a build tool is configured is not the product being built. `config`
    // as a whole dot-separated part covers `vite.config.ts`, `eslint.config.mjs`
    // and `tsup.config.ts` without touching a file merely named `configure.ts`.
    let parts: Vec<&str> = name.split('.').collect();
    if parts.len() > 2 && parts.contains(&"config") {
        return Some("build or tool configuration");
    }
    // A Gradle build script in Kotlin is a `.kts` file, so it is found as
    // source: `build.gradle.kts`, `settings.gradle.kts`, or one named for its
    // module as JUnit names `junit-jupiter-api.gradle.kts`.
    if name.ends_with(".gradle.kts") {
        return Some("build or tool configuration");
    }
    // The older dotfile spelling, `.eslintrc.js` and its kind. Splitting a name
    // that begins with a dot leaves an empty first part, so the tool's name is
    // the second one.
    if name.starts_with('.')
        && parts
            .get(1)
            .is_some_and(|tool| tool.len() > 3 && tool.ends_with("rc"))
    {
        return Some("build or tool configuration");
    }
    None
}

/// Directories a web application serves as they are, or keeps other people's
/// code in. A library copied into one of them is shipped, but it is not the
/// code under review.
const SERVED_DIRECTORIES: &[&str] = &[
    "assets",
    "bower_components",
    "lib",
    "libs",
    "plugins",
    "public",
    "static",
    "third-party",
    "third_party",
    "vendor",
    "vendors",
    "www",
    "wwwroot",
];

/// Browser libraries that are copied into applications far more often than
/// they are written in them. Matched as a whole dash- or dot-separated part of
/// a file name in a served directory, so `jquery.flot.js`,
/// `bootstrap-slider.js` and `swagger-ui-bundle.js` match and `chartUtils.js`
/// does not.
const LIBRARIES: &[&str] = &[
    "ace",
    "angular",
    "axios",
    "backbone",
    "bootstrap",
    "chart",
    "ckeditor",
    "codemirror",
    "d3",
    "datatables",
    "excanvas",
    "flot",
    "fontawesome",
    "handlebars",
    "highlight",
    "html5shiv",
    "jqvmap",
    "jquery",
    "knockout",
    "lodash",
    "mapael",
    "modernizr",
    "moment",
    "morris",
    "mustache",
    "pdfmake",
    "popper",
    "prism",
    "raphael",
    "redoc",
    "require",
    "respond",
    "select2",
    "slick",
    "socket.io",
    "sparkline",
    "summernote",
    "sweetalert",
    "swagger-ui",
    "swiper",
    "tinymce",
    "toastr",
    "underscore",
    "vfs_fonts",
    "zepto",
];

/// Whether a JavaScript file is someone else's code or a machine's, read from
/// its name, where it is served from and its first 64 KiB.
///
/// Measured on the 72 held-out RealVuln repositories: these files held 89 of
/// Supercov's security findings and none of the labelled weaknesses, and each
/// cost requests or failed over the request budget. A minified file is left
/// out wherever it is; a banner or library name only counts in a served or
/// vendor directory, because a first-party library keeps `@license` in `src/`
/// and a Python file can hold one very long HTML string.
fn third_party(path: &Path, file: &str) -> Option<&'static str> {
    let lower = file.to_ascii_lowercase();
    let (directories, name) = lower.rsplit_once('/').unwrap_or(("", lower.as_str()));
    if ![".js", ".mjs", ".cjs"]
        .iter()
        .any(|ext| name.ends_with(ext))
    {
        return None;
    }
    let mut head = Vec::new();
    std::io::Read::read_to_end(
        &mut std::io::Read::take(std::fs::File::open(path).ok()?, 64 << 10),
        &mut head,
    )
    .ok()?;
    let head = String::from_utf8_lossy(&head);
    let long: usize = head.lines().map(str::len).filter(|&n| n > 500).sum();
    if head.len() > 2000 && long * 2 > head.len() {
        return Some("minified code");
    }
    if !directories
        .split('/')
        .any(|segment| SERVED_DIRECTORIES.contains(&segment))
    {
        return None;
    }
    let banner = head
        .char_indices()
        .take_while(|(index, _)| *index < 2048)
        .last()
        .map_or("", |(index, c)| &head[..index + c.len_utf8()]);
    let licensed = banner.contains("/*!")
        || banner.contains("@license")
        || banner.contains("Licensed under the MIT")
        || banner.contains("Licensed under MIT")
        || banner.contains("Licensed under the Apache");
    let named = LIBRARIES.iter().any(|library| {
        name.match_indices(library).any(|(at, _)| {
            let before = name[..at].chars().last();
            let after = name[at + library.len()..].chars().next();
            before.is_none_or(|c| matches!(c, '.' | '-' | '_'))
                && after.is_some_and(|c| matches!(c, '.' | '-' | '_'))
        })
    });
    (licensed || named).then_some("vendored third-party library")
}

/// The conventional source directories a package actually has.
///
/// Read from the directory rather than joined blindly, so `Sources` matches on
/// a case-sensitive filesystem as well as on a case-insensitive one. A Python
/// package is any directory holding `__init__.py`, which is the flat layout PyPA
/// documents beside the `src` one.
fn conventional_directories(package: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(package) else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_ascii_lowercase();
        // A Python test package has an `__init__.py` too. Without this guard
        // `requests` reported `tests` as a source root beside `src`.
        if super::is_test_directory(&name) {
            continue;
        }
        if SOURCE_DIRECTORIES.contains(&name.as_str()) || path.join("__init__.py").is_file() {
            found.push(path);
        }
    }
    found
}

/// Go compiles every package under a module, wherever it sits, so a module root
/// is a source root for Go files and for nothing else. Scoping it by extension
/// keeps a `go.mod` at the top of a polyglot repository from claiming the rest
/// of the tree.
fn go_modules(packages: &BTreeSet<PathBuf>) -> Vec<PathBuf> {
    packages
        .iter()
        .filter(|package| package.join("go.mod").is_file())
        .cloned()
        .collect()
}

fn relative(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .replace('\\', "/")
}

/// What a coverage run of this project would measure: the engine's own source
/// scope, which is where the rules for one framework's layout live.
///
/// The rules here once stood alone and fell behind it. An App Router handler
/// at `app/api/wordpress/test/route.ts` was measured by coverage and left out
/// here as a test, in a security review of the handlers that take a caller's
/// credentials; `components/` and `proxy.ts` were source to coverage and
/// unclassified here; and a root a project named was still second-guessed by
/// its own name. A file coverage measures is first-party code a test can run,
/// so it is in scope unless it is generated or somebody else's.
struct Measured {
    files: BTreeSet<PathBuf>,
    roots: Vec<PathBuf>,
}

impl Measured {
    /// Nothing, when the engine cannot read the tree: the rules below decide.
    fn of(root: &Path, configured: Option<&[String]>) -> Self {
        match supercov_engine::source_discovery::discover_source_scope(root, configured) {
            Ok(found) => Self {
                files: found.source_files.iter().map(|f| root.join(f)).collect(),
                roots: found.source_roots.iter().map(|r| root.join(r)).collect(),
            },
            Err(_) => Self {
                files: BTreeSet::new(),
                roots: Vec::new(),
            },
        }
    }
}

/// Decide the scope for a set of already-discovered source files.
///
/// `files` are absolute paths the caller found; this classifies them and says
/// which roots it used. Configured roots switch the mode to explicit, where
/// anything outside them is excluded rather than left ambiguous.
pub fn classify(root: &Path, files: &[PathBuf], configured: Option<&[String]>) -> Scope {
    let explicit = configured.is_some_and(|roots| !roots.is_empty());
    let measured = Measured::of(root, configured.filter(|_| explicit));
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
            // A .NET project keeps its sources in the project directory itself,
            // so the directory is a root whatever it also contains. Without
            // this, `System.Reactive.Async/Internal` became the only root
            // because `Internal` collides with Go's `internal`, and every file
            // beside the project file was left unclassified.
            if package != root && variable_manifest(package) == Some(true) {
                roots.insert(package.clone());
                continue;
            }
            let mut candidates: Vec<PathBuf> = conventional_directories(package);
            candidates.extend(declared_entry_points(package));
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
    let modules = if explicit {
        Vec::new()
    } else {
        go_modules(&packages)
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
        // Where a file sits can say it is a test, a script or an example; a
        // coverage run that measures it has already settled that it is not.
        let measured = measured.files.contains(path);
        let by_place = !measured;
        if let Some(skipped) = super::skipped_path(&file)
            .filter(|skipped| by_place || *skipped != super::Skipped::Test)
        {
            entries.push(entry(Status::Excluded, skipped.reason()));
        } else if let Some(reason) = beside_the_product(&file).filter(|_| by_place) {
            entries.push(entry(Status::Excluded, reason));
        } else if let Some(reason) = third_party(path, &file) {
            entries.push(entry(Status::Excluded, reason));
        } else if measured
            || roots.iter().any(|dir| path.starts_with(dir))
            || (path.extension().is_some_and(|e| e == "go")
                && modules.iter().any(|module| path.starts_with(module)))
        {
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
    // The roots coverage measures files under are named with the ones found
    // here, since those files are in scope: `components` and `proxy.ts` of a
    // Next.js application, which no other ecosystem's conventions name. One
    // it measures nothing under is a directory it only looked in, such as the
    // `src` of a package kept as a test fixture.
    let roots: BTreeSet<&PathBuf> = roots
        .iter()
        .chain(
            measured
                .roots
                .iter()
                .filter(|dir| measured.files.iter().any(|file| file.starts_with(dir))),
        )
        .collect();
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

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(files: &[(&str, &str)]) -> PathBuf {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "supercov-scope-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        for (file, text) in files {
            let path = root.join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        root
    }

    /// A manifest's declared locations become roots: a file in a directory
    /// of its own names the directory, one at the package root only itself;
    /// build output, absolute, parent and missing paths name nothing.
    #[test]
    fn declared_entry_points_name_their_directories() {
        let root = directory(&[
            (
                "package.json",
                r#"{"main":"lib/index.js","bin":{"x":"bin/x.js"},"browser":"./dist/b.js","module":"/abs.js","exports":{"./runtime/*":"./runtime/*.mjs","./up":"../up.js","./all":"*","./gone":"./gone.js","deep":{"a":{"b":{"c":{"d":"too/deep.js"}}}}}}"#,
            ),
            ("lib/index.js", ""),
            ("bin/x.js", ""),
            ("runtime/a.mjs", ""),
            ("too/deep.js", ""),
            ("dist/b.js", ""),
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\npathological = true\n[[bin]]\npath = \"src/main.rs\"\n",
            ),
            ("src/main.rs", ""),
            ("build.rs", ""),
        ]);
        let mut found = declared_entry_points(&root)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        found.sort();
        assert_eq!(found, ["bin", "build.rs", "lib", "runtime", "src"]);
        std::fs::remove_dir_all(root).unwrap();
    }

    /// Manifests named after their project, and which files declare a package.
    #[test]
    fn variable_manifests_and_package_files_are_told_by_name() {
        for (file, expected) in [
            ("api.gradle.kts", Some(false)),
            ("api.gradle", Some(false)),
            ("App.fsproj", Some(true)),
            ("App.vbproj", Some(true)),
            ("notes.txt", None),
        ] {
            let root = directory(&[(file, "")]);
            assert_eq!(variable_manifest(&root), expected, "{file}");
            std::fs::remove_dir_all(root).unwrap();
        }
        for (path, declares) in [
            ("a/Cargo.toml", true),
            ("a/junit.gradle.kts", true),
            ("App.CSPROJ", true),
            ("All.sln", true),
            ("notes.txt", false),
        ] {
            assert_eq!(declares_a_package(path), declares, "{path}");
        }
    }

    /// A root manifest's members: npm's workspaces in either form, Cargo's
    /// across lines, a trailing `*` read as every directory under it.
    #[test]
    fn declared_members_are_read_from_either_ecosystem() {
        let root = directory(&[
            (
                "package.json",
                r#"{"workspaces":{"packages":["packages/*","tools","missing"]}}"#,
            ),
            ("packages/a/index.js", ""),
            ("packages/b/index.js", ""),
            ("packages/file.txt", ""),
            ("tools/index.js", ""),
            (
                "Cargo.toml",
                "[package]\nname = \"x\"\n[workspace]\nmembers = [\n  \"crates/one\",\n  \"crates/*\",\n]\n[dependencies]\nserde = \"1\"\n",
            ),
            ("crates/one/Cargo.toml", ""),
            ("crates/two/Cargo.toml", ""),
        ]);
        let mut found = declared_members(&root)
            .into_iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        found.sort();
        assert_eq!(
            found,
            [
                "crates/one",
                "crates/two",
                "packages/a",
                "packages/b",
                "tools"
            ]
        );
        let flat = directory(&[
            ("package.json", r#"{"workspaces":["tools"]}"#),
            ("tools/x.js", ""),
        ]);
        assert_eq!(declared_members(&flat).len(), 1);
        std::fs::remove_dir_all(root).unwrap();
        std::fs::remove_dir_all(flat).unwrap();
    }

    /// Tool configuration by its name, and a served file left out as a
    /// vendored library by its licence banner, its name or its minification.
    #[test]
    fn configuration_and_vendored_files_are_told_by_name_and_text() {
        for (file, configuration) in [
            ("vite.config.ts", true),
            ("build.gradle.kts", true),
            (".eslintrc.js", true),
            (".babelrc.json", true),
            (".rc.js", false),
            (".eslint.js", false),
            ("configure.ts", false),
        ] {
            assert_eq!(
                beside_the_product(file) == Some("build or tool configuration"),
                configuration,
                "{file}"
            );
        }
        let root = directory(&[
            ("public/js/banner.js", "/*! some library v1 */\nvar a=1;\n"),
            ("public/js/jquery.min.js", "var j = 1;\n"),
            ("public/js/myjquery.js", "var j = 1;\n"),
            ("public/js/app.js", "export const app = 1;\n"),
            ("public/js/blob.js", &format!("{}\n", "x".repeat(3000))),
            ("src/jquery.min.js", "var j = 1;\n"),
        ]);
        let vendored = |file: &str| third_party(&root.join(file), file);
        assert_eq!(
            vendored("public/js/banner.js"),
            Some("vendored third-party library")
        );
        assert_eq!(
            vendored("public/js/jquery.min.js"),
            Some("vendored third-party library")
        );
        assert_eq!(
            vendored("public/js/myjquery.js"),
            None,
            "a name inside another word"
        );
        assert_eq!(vendored("public/js/app.js"), None);
        assert_eq!(vendored("public/js/blob.js"), Some("minified code"));
        assert_eq!(
            vendored("src/jquery.min.js"),
            None,
            "not a served directory"
        );
        std::fs::remove_dir_all(root).unwrap();
    }
}
