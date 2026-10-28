//! Deterministic first-party JavaScript/TypeScript source discovery.
//!
//! Discovery defines the coverage denominator. The walker never follows
//! links and turns unclassified first-party files into explicit blockers.

use crate::fault_fs as fs;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const GENERATED_DIRECTORIES: &[&str] = &[
    ".cache",
    "generated",
    ".git",
    ".mcdc-pool",
    ".next",
    ".nuxt",
    ".output",
    ".supercov",
    "build",
    "coverage",
    "dist",
    "node_modules",
    "out",
    "playwright-report",
    "results",
    "test-results",
    "vendor",
];
/// Whether a directory called `target` is a build tool's output rather than a
/// project's own code.
///
/// Cargo, Maven, sbt and Leiningen write there, but the name is not theirs: a
/// Flask application kept its whole package in `target/`, and dropping the
/// directory by name left the repository with no source at all. It is output
/// when a build that writes there is declared beside it, or when Cargo's own
/// markers are inside it, which also covers a `CARGO_TARGET_DIR` placed
/// elsewhere.
pub fn target_is_build_output(directory: &Path) -> bool {
    const BUILDS: &[&str] = &[
        "Cargo.toml",
        "pom.xml",
        "build.gradle",
        "build.gradle.kts",
        "settings.gradle",
        "settings.gradle.kts",
        "build.sbt",
        "project.clj",
    ];
    const MARKERS: &[&str] = &["CACHEDIR.TAG", ".rustc_info.json"];
    directory
        .parent()
        .is_some_and(|parent| BUILDS.iter().any(|name| parent.join(name).is_file()))
        || MARKERS.iter().any(|name| directory.join(name).is_file())
}

const SOURCE_DIRECTORIES: &[&str] = &[
    "app",
    "src",
    "lib",
    "server",
    "client",
    "functions",
    "api",
    "components",
    "pages",
];
/// The files Next.js reads from a project's root by name. `proxy` is what
/// Next 16 calls `middleware`.
const NEXT_ROOT_FILES: &[&str] = &[
    "proxy",
    "middleware",
    "instrumentation",
    "instrumentation-client",
];
/// The file names the App Router gives a meaning to under `app/`.
const NEXT_APP_FILES: &[&str] = &[
    "page",
    "layout",
    "route",
    "loading",
    "error",
    "global-error",
    "not-found",
    "template",
    "default",
    "forbidden",
    "unauthorized",
    "icon",
    "apple-icon",
    "opengraph-image",
    "twitter-image",
    "sitemap",
    "robots",
    "manifest",
];
const PACKAGE_PARENTS: &[&str] = &["apps", "packages", "services", "workspaces"];
const TEST_DIRECTORIES: &[&str] = &[
    "__tests__",
    "test",
    "tests",
    "spec",
    "specs",
    "e2e",
    "fixture",
    "fixtures",
    "mock",
    "mocks",
    "__mocks__",
];
const CONFIG_TOOLS: &[&str] = &[
    "babel",
    "eslint",
    "graphql",
    "jest",
    "next",
    "nuxt",
    "playwright",
    "postcss",
    "prettier",
    "remix",
    "rollup",
    "stylelint",
    "tailwind",
    "tsup",
    "vite",
    "vitest",
    "webpack",
];
const DOT_CONFIG_TOOLS: &[&str] = &["babel", "eslint", "graphql", "prettier", "stylelint"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScopeStatus {
    Included,
    Excluded,
    Ambiguous,
}

/// Scope reason for a bundler's output found in the tree (see `built_asset`).
pub const BUILT_ASSET_REASON: &str = "built asset";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceScopeEntry {
    pub file: String,
    pub status: SourceScopeStatus,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_root: Option<String>,
}

impl SourceScopeEntry {
    /// A file the wrapped command generates rather than one anyone edits: it
    /// is never instrumented or cached, and a bundler renames it on every
    /// build, so it must not feed the source fingerprint.
    pub fn is_generated_output(&self) -> bool {
        self.status == SourceScopeStatus::Excluded && self.reason == BUILT_ASSET_REASON
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SourceScopeMode {
    Automatic,
    Explicit,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceScope {
    pub version: u32,
    pub mode: SourceScopeMode,
    pub roots: Vec<String>,
    pub entries: Vec<SourceScopeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceLimitation {
    pub id: String,
    pub kind: String,
    pub file: String,
    pub line: usize,
    pub column: usize,
    pub source: String,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DiscoveredSourceScope {
    pub source_files: Vec<String>,
    pub source_roots: Vec<String>,
    pub scope: SourceScope,
    pub limitations: Vec<SourceLimitation>,
}

#[derive(Debug)]
pub enum SourceDiscoveryError {
    Io { path: PathBuf, source: io::Error },
    NonUtf8Path(PathBuf),
    InvalidRoot(PathBuf),
    OutsideRoot(PathBuf),
}

impl std::fmt::Display for SourceDiscoveryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "source path is not valid UTF-8: {}",
                    path.display()
                )
            }
            Self::InvalidRoot(path) => write!(
                formatter,
                "source root is not a regular file or directory: {}",
                path.display()
            ),
            Self::OutsideRoot(path) => write!(
                formatter,
                "source root is outside the project: {}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for SourceDiscoveryError {}

fn io_error(path: &Path, source: io::Error) -> SourceDiscoveryError {
    SourceDiscoveryError::Io {
        path: path.to_owned(),
        source,
    }
}

/// The variable that names first-party source roots, for every frontend.
pub const SOURCE_ROOTS_VARIABLE: &str = "SUPERCOV_SOURCE_ROOTS";

/// Why a file a frontend would otherwise measure was left out.
pub const OUTSIDE_EXPLICIT_ROOTS: &str = "outside explicit source roots";

/// The first-party source roots named in `SUPERCOV_SOURCE_ROOTS`:
/// comma-separated, trimmed, empties dropped. `None` when the variable is unset
/// or names nothing, which leaves discovery automatic.
///
/// Every frontend reads the roots through here. It used to be read on the
/// JavaScript path alone while the documentation described it without a
/// language, so a Python project that set it had every file measured anyway --
/// a vendored tree beside flat-layout sources came out at 660 files instead of
/// fifteen, with nothing to say the variable had been ignored.
pub fn configured_source_roots(environment: &BTreeMap<String, String>) -> Option<Vec<String>> {
    let roots = environment
        .get(SOURCE_ROOTS_VARIABLE)?
        .split(',')
        .map(str::trim)
        .filter(|root| !root.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    (!roots.is_empty()).then_some(roots)
}

/// Fold the roots a run was measured under into its execution configuration,
/// the part of a run's identity that records how it was asked to run. Two runs
/// over different roots measure different denominators and are not the same
/// run; nothing is appended when no roots were named, so every existing
/// fingerprint stays as it was.
pub fn fold_roots_into_configuration(configuration: &mut Vec<u8>, roots: Option<&[String]>) {
    let Some(roots) = roots.filter(|roots| !roots.is_empty()) else {
        return;
    };
    configuration.push(0);
    configuration
        .extend_from_slice(format!("{SOURCE_ROOTS_VARIABLE}={}", roots.join(",")).as_bytes());
}

/// Explicit source roots resolved against a project root. Each is an existing
/// directory or a single file, so a flat layout can name the modules it keeps
/// at the root one by one. A root that does not exist names nothing to measure
/// and is skipped; one that is neither a file nor a directory is refused, since
/// a symlink root would reach outside what the project owns.
#[derive(Debug, Clone)]
pub struct ExplicitSourceRoots {
    root: PathBuf,
    roots: Vec<PathBuf>,
    configured: Vec<String>,
}

impl ExplicitSourceRoots {
    pub fn resolve(root: &Path, configured: &[String]) -> Result<Self, SourceDiscoveryError> {
        let root = lexical_normalize(root);
        let roots = existing_roots(configured.iter().map(|value| resolve(&root, value)), true)?;
        Ok(Self {
            root,
            roots,
            configured: configured.to_vec(),
        })
    }

    /// Read the roots from the environment, if any are named.
    pub fn from_environment(
        root: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, SourceDiscoveryError> {
        configured_source_roots(environment)
            .map(|configured| Self::resolve(root, &configured))
            .transpose()
    }

    /// Read the roots named against `project`, resolved inside `workspace` --
    /// the isolated copy Go, the JVM and Rust measure. The copy mirrors the
    /// project, so a relative root names the same file in both. An absolute one
    /// names the original tree, which nothing in the copy lies under, so it is
    /// rebased onto the copy; without that it would match nothing and read as a
    /// typo. The roots are still quoted as they were written.
    pub fn from_environment_in(
        project: &Path,
        workspace: &Path,
        environment: &BTreeMap<String, String>,
    ) -> Result<Option<Self>, SourceDiscoveryError> {
        let Some(configured) = configured_source_roots(environment) else {
            return Ok(None);
        };
        let project = lexical_normalize(project);
        let rebased = configured
            .iter()
            .map(|value| {
                let path = Path::new(value);
                if !path.is_absolute() {
                    return value.clone();
                }
                lexical_normalize(path)
                    .strip_prefix(&project)
                    .map(|rest| {
                        let rest = rest.to_string_lossy().into_owned();
                        if rest.is_empty() { ".".into() } else { rest }
                    })
                    .unwrap_or_else(|_| value.clone())
            })
            .collect::<Vec<_>>();
        let mut roots = Self::resolve(workspace, &rebased)?;
        roots.configured = configured;
        Ok(Some(roots))
    }

    /// Whether a project-relative file lies under these roots: inside a
    /// directory root, or the very file a file root names.
    pub fn contains(&self, relative: &str) -> bool {
        root_contains(&self.roots, &lexical_normalize(&self.root.join(relative)))
    }

    /// Refuse roots that left a frontend nothing to measure. A typo in the
    /// variable must not become a green run over zero files -- the failure mode
    /// most likely to pass unnoticed in an agent loop.
    pub fn refuse_if_empty(&self, remaining: usize, language: &str) -> Result<(), String> {
        if remaining > 0 {
            return Ok(());
        }
        Err(self.nothing_left(language))
    }

    /// What to say of roots that left no `language` source to measure.
    pub fn nothing_left(&self, language: &str) -> String {
        let missing = self
            .configured
            .iter()
            .filter(|value| fs::symlink_metadata(resolve(&self.root, value)).is_err())
            .cloned()
            .collect::<Vec<_>>();
        let mut message = format!(
            "{SOURCE_ROOTS_VARIABLE}={} left no {language} source to measure",
            self.configured.join(",")
        );
        if !missing.is_empty() {
            let verb = if missing.len() == 1 { "does" } else { "do" };
            message.push_str(&format!("; {} {verb} not exist", missing.join(", ")));
        }
        message.push_str("; name the directories or files that hold your own code");
        message
    }

    /// The scope a frontend that has no scope of its own records when explicit
    /// roots shaped what it measures: the files kept, and the files the roots
    /// left out. Without it, honouring the variable would be as invisible as
    /// ignoring it -- the complaint that started this -- since `runs <id>
    /// scope` would have nothing to show.
    pub fn scope(
        &self,
        included: &[String],
        excluded: &[(String, &'static str)],
    ) -> Result<SourceScope, SourceDiscoveryError> {
        let mut entries = included
            .iter()
            .map(|file| SourceScopeEntry {
                file: file.clone(),
                status: SourceScopeStatus::Included,
                reason: "explicit source root".into(),
                package_root: None,
            })
            .chain(
                excluded
                    .iter()
                    .filter(|(_, reason)| *reason == OUTSIDE_EXPLICIT_ROOTS)
                    .map(|(file, reason)| SourceScopeEntry {
                        file: file.clone(),
                        status: SourceScopeStatus::Excluded,
                        reason: (*reason).into(),
                        package_root: None,
                    }),
            )
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.file.cmp(&right.file));
        Ok(SourceScope {
            version: 1,
            mode: SourceScopeMode::Explicit,
            roots: self.local_roots()?,
            entries,
        })
    }

    /// The roots as project-relative paths, as the recorded scope names them.
    pub fn local_roots(&self) -> Result<Vec<String>, SourceDiscoveryError> {
        self.roots
            .iter()
            .map(|path| local_path(&self.root, path))
            .collect()
    }

    /// Split `sources` into the files these roots keep and the ones they leave
    /// out, recording why each was left out. A frontend's own exclusions --
    /// tests, environments, build output -- have already been applied, so a
    /// root narrows what is measured and never pulls a test file back in.
    pub fn narrow<T>(
        &self,
        sources: &mut Vec<T>,
        excluded: &mut Vec<(String, &'static str)>,
        file: impl Fn(&T) -> &str,
    ) {
        let (kept, outside): (Vec<T>, Vec<T>) = sources
            .drain(..)
            .partition(|source| self.contains(file(source)));
        excluded.extend(
            outside
                .iter()
                .map(|source| (file(source).to_owned(), OUTSIDE_EXPLICIT_ROOTS)),
        );
        *sources = kept;
    }
}

/// The configured or discovered roots that exist, each a regular file or
/// directory. Shared by explicit and automatic discovery so the two cannot
/// disagree about what counts as a root. A root someone named that is
/// anything else is refused; a conventional one discovery merely guessed
/// (`lib` linked to `src`, a `main` entry linked into `dist/`) is passed
/// over, as the copy the run measures passes over a link out of the project.
fn existing_roots(
    candidates: impl IntoIterator<Item = PathBuf>,
    named: bool,
) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    let mut existing = BTreeSet::new();
    for path in candidates {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_dir() => {
                existing.insert(path);
            }
            Ok(_) if !named => {}
            Ok(_) => return Err(SourceDiscoveryError::InvalidRoot(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&path, error)),
        }
    }
    Ok(existing.into_iter().collect())
}

/// Whether `path` lies under one of `roots`: inside a directory root, or equal
/// to a file root.
fn root_contains(roots: &[PathBuf], path: &Path) -> bool {
    roots.iter().any(|directory| {
        fs::symlink_metadata(directory)
            .map(|metadata| {
                if metadata.file_type().is_dir() {
                    within(directory, path)
                } else {
                    directory == path
                }
            })
            .unwrap_or(false)
    })
}

pub(crate) fn lexical_normalize(path: &Path) -> PathBuf {
    let mut output = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !output.pop() {
                    output.push(component.as_os_str());
                }
            }
            _ => output.push(component.as_os_str()),
        }
    }
    output
}

fn resolve(root: &Path, value: impl AsRef<Path>) -> PathBuf {
    let value = value.as_ref();
    let joined;
    let path = if value.is_absolute() {
        value
    } else {
        joined = root.join(value);
        &joined
    };
    lexical_normalize(path)
}

fn local_path(root: &Path, path: &Path) -> Result<String, SourceDiscoveryError> {
    let local = path
        .strip_prefix(root)
        .map_err(|_| SourceDiscoveryError::OutsideRoot(path.to_owned()))?;
    if local.as_os_str().is_empty() {
        return Ok(".".into());
    }
    local
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| SourceDiscoveryError::NonUtf8Path(path.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

/// [`local_path`] of a path a walk of `root` found: the walk read each name
/// it went through as UTF-8, under the root.
fn walked(root: &Path, path: &Path) -> String {
    local_path(root, path).expect("a walked path is UTF-8 under its root")
}

fn generated_directory(name: &str) -> bool {
    GENERATED_DIRECTORIES.contains(&name)
}

fn owned_workspace_store(path: &Path) -> bool {
    crate::workspace::owned_workspace_path(path)
}

/// A directory carrying its own `.git` entry is another checkout — a nested
/// clone, a submodule, or an agent worktree such as `.claude/worktrees/*` —
/// not this project's source. Treating its files as ambiguous first-party
/// code turned one real project's report into 1,032 blocking limitations.
#[track_caller]
fn nested_checkout(path: &Path) -> bool {
    fs::symlink_metadata(path.join(".git")).is_ok()
}

/// A hidden directory at the project root is tool state (.shopify, .vercel,
/// .idea, .claude, ...) by convention, never application source. Nested
/// hidden directories keep their normal treatment so a source tree that
/// happens to contain one is not silently truncated.
/// A hashed bundle (`app-embed-Be-aUw9g.js`) inside an assets/static/public
/// directory is a bundler's output, not source: a theme extension's `assets/`
/// receives its Vite build, and every such bundle was an ambiguous blocker.
fn built_asset(file: &str) -> bool {
    let mut segments = file.rsplit('/');
    let name = segments.next().expect("a split yields a segment");
    let in_asset_directory =
        segments.any(|segment| matches!(segment, "assets" | "static" | "public"));
    let Some(stem) = name
        .strip_suffix(".js")
        .or_else(|| name.strip_suffix(".mjs"))
        .or_else(|| name.strip_suffix(".cjs"))
    else {
        return false;
    };
    // Bundler hashes are eight base64url characters, which may themselves
    // contain '-', so take the suffix by length rather than splitting on it.
    if stem.len() < 10 || stem.as_bytes()[stem.len() - 9] != b'-' {
        return false;
    }
    let hash = &stem[stem.len() - 8..];
    let looks_hashed = hash
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
        && (hash.bytes().any(|byte| byte.is_ascii_digit())
            || (hash.bytes().any(|byte| byte.is_ascii_uppercase())
                && hash.bytes().any(|byte| byte.is_ascii_lowercase())));
    in_asset_directory && looks_hashed
}

fn root_tool_directory(root: &Path, path: &Path) -> bool {
    path.parent() == Some(root)
        && path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with('.'))
}

fn source_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    [
        ".js", ".jsx", ".ts", ".tsx", ".cjs", ".cjsx", ".cts", ".ctsx", ".mjs", ".mjsx", ".mts",
        ".mtsx",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

fn declaration_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    lower.ends_with(".d.ts") || lower.ends_with(".d.cts") || lower.ends_with(".d.mts")
}

fn test_or_fixture(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    if lower
        .split('/')
        .any(|segment| TEST_DIRECTORIES.contains(&segment))
    {
        return true;
    }
    lower
        .split(['/', '_', '.', '-'])
        .any(|part| matches!(part, "test" | "spec"))
}

fn depends_on_next(manifest: &Value) -> bool {
    ["dependencies", "devDependencies", "peerDependencies"]
        .iter()
        .any(|field| {
            manifest
                .get(field)
                .is_some_and(|all| all.get("next").is_some())
        })
}

/// A file the App Router serves. Under `app/` a directory is a URL segment,
/// so `app/api/wordpress/test/route.ts` is the handler of `/api/wordpress/test`
/// and not a test: five such "test connection" endpoints of one application
/// were left out of what it measured. A folder that starts with `_` is private
/// to the project and is not a segment, which keeps `__tests__` a test folder.
fn next_app_file(app_directories: &[PathBuf], path: &Path) -> bool {
    let Some(local) = app_directories
        .iter()
        .find_map(|directory| path.strip_prefix(directory).ok())
    else {
        return false;
    };
    let Some(name) = local.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let conventional = name.split_once('.').is_some_and(|(stem, extension)| {
        NEXT_APP_FILES.contains(&stem) && !extension.contains('.')
    });
    conventional
        && local.parent().is_none_or(|folders| {
            folders
                .components()
                .all(|folder| !folder.as_os_str().to_string_lossy().starts_with('_'))
        })
}

/// The path a named root leaves to judge: the part below it. Whoever names
/// `scripts` or `src/test-utils` as a source root has said what it holds, so
/// only what lies deeper is still guessed at. A root that is a file names that
/// file, and nothing about it is guessed.
fn below_named_root<'a>(roots: &[PathBuf], path: &'a Path) -> Option<&'a Path> {
    roots
        .iter()
        .filter_map(|root| path.strip_prefix(root).ok())
        .min_by_key(|local| local.components().count())
}

/// Code kept beside a project's packages without being part of one, by the
/// directory at the top of the project it lies in: the documentation's
/// build script, example programs, benchmarks, fuzz harnesses. A directory of such a name
/// deeper down can be a package of the product and is left to be measured.
///
/// The frontends that walk a tree for every source file (Python, Ruby)
/// counted these. On h11 `docs/`, `examples/`, `bench/` made six of eighteen
/// measured files, none of which a test runs, and the package's own 99% of
/// lines read 69%; click measured thirteen such files of thirty. Each of
/// eight projects looked at had at least one of the three.
pub fn beside_the_product(file: &str) -> Option<&'static str> {
    let (first, _) = file.split_once('/')?;
    match first.to_ascii_lowercase().as_str() {
        "docs" | "doc" | "documentation" => Some("documentation"),
        "examples" | "example" => Some("example"),
        "bench" | "benches" | "benchmark" | "benchmarks" => Some("benchmark"),
        "fuzz" | "fuzzing" => Some("fuzz harness"),
        _ => None,
    }
}

/// Take what lies beside the product out of `sources`, saying why. For a
/// project that names no roots: a root someone named is what they say it is.
pub fn leave_out_what_lies_beside(
    sources: &mut Vec<String>,
    excluded: &mut Vec<(String, &'static str)>,
) {
    sources.retain(|file| match beside_the_product(file) {
        Some(reason) => {
            excluded.push((file.clone(), reason));
            false
        }
        None => true,
    });
}

/// The scope a frontend records when nothing named its roots: each file it
/// measures, and each file it left out with why. `runs <id> scope` lists it,
/// and for a Go, Rust or JVM run had nothing to list: the summary offered the
/// command and the command answered that the run held no inventory.
pub fn automatic_scope(
    measured: impl IntoIterator<Item = (String, String)>,
    left_out: impl IntoIterator<Item = (String, String)>,
) -> SourceScope {
    let entry = |status: SourceScopeStatus| {
        move |(file, reason)| SourceScopeEntry {
            file,
            status: status.clone(),
            reason,
            package_root: None,
        }
    };
    let mut entries = measured
        .into_iter()
        .map(entry(SourceScopeStatus::Included))
        .chain(left_out.into_iter().map(entry(SourceScopeStatus::Excluded)))
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.file.cmp(&right.file));
    SourceScope {
        version: 1,
        mode: SourceScopeMode::Automatic,
        roots: vec![".".into()],
        entries,
    }
}

/// A benchmark: Vitest's `*.bench.ts` beside the code it measures, or a file
/// under a `bench`/`benchmarks` directory. It runs the product; it is not the
/// product. jshttp/cookie keeps four `src/*.bench.ts` files, which were
/// counted as application source and led the list of code no test ran.
fn benchmark(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    let (directories, name) = lower.rsplit_once('/').unwrap_or(("", lower.as_str()));
    directories
        .split('/')
        .any(|segment| matches!(segment, "bench" | "benches" | "benchmark" | "benchmarks"))
        || name
            .split('.')
            .skip(1)
            .any(|part| matches!(part, "bench" | "benchmark"))
}

/// Example code at the top of the project or of a package: uuid keeps nine
/// example apps under `examples/`, which its tsconfig leaves out and which
/// were counted as application source, putting uuid at 32% of lines. A
/// directory of that name deeper in the source (`app/examples/page.tsx`) can
/// be the product and stays in.
fn example(file: &str, package_root: Option<&str>) -> bool {
    let local = package_root
        .and_then(|package| file.strip_prefix(package))
        .and_then(|rest| rest.strip_prefix('/'))
        .unwrap_or(file);
    local.split_once('/').is_some_and(|(first, _)| {
        matches!(first.to_ascii_lowercase().as_str(), "example" | "examples")
    })
}

fn tool_script(file: &str) -> bool {
    file.to_ascii_lowercase()
        .split('/')
        .any(|segment| segment == "scripts")
}

fn config_file(file: &str) -> bool {
    let lower = file.to_ascii_lowercase();
    let name = lower.rsplit('/').next().unwrap_or(&lower);
    source_file(name)
        && (CONFIG_TOOLS
            .iter()
            .any(|tool| name.starts_with(&format!("{tool}.config.")))
            || DOT_CONFIG_TOOLS
                .iter()
                .any(|tool| name.starts_with(&format!(".{tool}rc.")))
            || (!lower.contains('/')
                && (name.contains(".config.")
                    // wdio.conf.js, karma.conf.js, protractor.conf.js
                    || name.contains(".conf.")
                    || name.starts_with("build.")
                    || name.starts_with("gulpfile.")
                    || name.starts_with("gruntfile."))))
}

fn read_directory(path: &Path) -> Result<Vec<fs::DirEntry>, SourceDiscoveryError> {
    let mut entries = fs::read_dir(path)
        .map_err(|error| io_error(path, error))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io_error(path, error))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    Ok(entries)
}

/// Whether the repository around a project ignores the project's own
/// directory: a copy kept under another checkout's `tmp/` or `.supercov/`.
/// That repository's `.gitignore` files describe what it does not track, which
/// is all of this project; applying them left the project with no source.
pub(crate) fn ignored_by_enclosing_repository(root: &Path) -> bool {
    if root.join(".git").exists() {
        return false;
    }
    let Some(repository) = root
        .ancestors()
        .skip(1)
        .position(|ancestor| ancestor.join(".git").exists())
    else {
        return false;
    };
    // Outermost first, so a deeper file's decision wins as it does in git.
    root.ancestors()
        .skip(1)
        .take(repository + 1)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .fold(false, |ignored, directory| {
            let (rules, _) = ignore::gitignore::Gitignore::new(directory.join(".gitignore"));
            match rules.matched_path_or_any_parents(root, true) {
                ignore::Match::Ignore(_) => true,
                ignore::Match::Whitelist(_) => false,
                ignore::Match::None => ignored,
            }
        })
}

/// Source files under a root, in name order.
///
/// A file the project's own `.gitignore` excludes is not its source: recording
/// Actual counted `packages/loot-core/lib-dist/browser/kcab.worker.dev.js`, a
/// 5 MB bundle its build writes and git ignores, as application code, and
/// instrumented it. Only the project's `.gitignore` files are read, and only
/// inside a git repository; a user's global excludes and `.git/info/exclude`
/// differ between machines, and a run must count the same files on each.
fn files_under(
    root: &Path,
    directory: &Path,
    output: &mut Vec<PathBuf>,
) -> Result<(), SourceDiscoveryError> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| io_error(directory, error))?;
    if !metadata.file_type().is_dir() {
        return Err(SourceDiscoveryError::InvalidRoot(directory.to_owned()));
    }
    let project = root.to_owned();
    let walker = ignore::WalkBuilder::new(directory)
        .standard_filters(false)
        .git_ignore(true)
        .parents(!ignored_by_enclosing_repository(directory))
        .require_git(true)
        .follow_links(false)
        .sort_by_file_name(|left, right| left.cmp(right))
        // The walker never asks about the directory it starts from: `ignore`
        // keeps every entry of depth 0 before it consults this filter.
        .filter_entry(move |entry| {
            !entry.file_type().is_some_and(|kind| kind.is_dir())
                || entry.file_name().to_str().is_some_and(|name| {
                    !generated_directory(name)
                        && !owned_workspace_store(entry.path())
                        && !nested_checkout(entry.path())
                        && !root_tool_directory(&project, entry.path())
                })
        })
        .build();
    // Never empty: the walk yields the directory it starts from first.
    walker.into_iter().try_for_each(|entry| {
        let entry =
            entry.map_err(|error| io_error(directory, io::Error::other(error.to_string())))?;
        // Files alone: not a link, a directory, or an entry with no type.
        if !entry
            .file_type()
            .is_some_and(|kind| !kind.is_symlink() && kind.is_file())
        {
            return Ok(());
        }
        let name = entry
            .file_name()
            .to_str()
            .ok_or_else(|| SourceDiscoveryError::NonUtf8Path(entry.path().to_owned()))?;
        if source_file(name) {
            output.push(entry.into_path());
        }
        Ok(())
    })
}

#[track_caller]
fn read_json(path: &Path) -> Option<Value> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn package_directories(root: &Path) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    fn visit(
        root: &Path,
        directory: &Path,
        depth: usize,
        found: &mut BTreeSet<PathBuf>,
    ) -> Result<(), SourceDiscoveryError> {
        if depth > 5 {
            return Ok(());
        }
        for entry in read_directory(directory)? {
            let name = entry
                .file_name()
                .into_string()
                .map_err(|name| SourceDiscoveryError::NonUtf8Path(directory.join(name)))?;
            let path = entry.path();
            let file_type = fs::file_type(&entry).map_err(|error| io_error(&path, error))?;
            // An entry's type is the link's own, never its target's: a link
            // to a directory is no directory here.
            if !file_type.is_dir()
                || generated_directory(&name)
                || owned_workspace_store(&path)
                || nested_checkout(&path)
                || root_tool_directory(root, &path)
            {
                continue;
            }
            let local = walked(root, &path);
            let under_package_parent = local
                .split('/')
                .any(|segment| PACKAGE_PARENTS.contains(&segment));
            if path.join("package.json").is_file() && (depth == 0 || under_package_parent) {
                found.insert(path.clone());
            }
            visit(root, &path, depth + 1, found)?;
        }
        Ok(())
    }

    let mut found = BTreeSet::from([root.to_owned()]);
    visit(root, root, 0, &mut found)?;
    for directory in declared_workspace_packages(root)? {
        found.insert(directory);
    }
    Ok(found.into_iter().collect())
}

/// Packages the project itself declares: `workspaces` in the root
/// package.json (array or `{ "packages": [...] }`) and `packages:` in
/// pnpm-workspace.yaml. The manifest is the most authoritative statement of
/// where packages live, and it routinely names directories outside the
/// conventional parents (a real project keeps its Shopify extensions under
/// `app_extensions/*`; every file there was an ambiguous blocker).
fn declared_workspace_packages(root: &Path) -> Result<Vec<PathBuf>, SourceDiscoveryError> {
    let mut patterns = Vec::new();
    let manifest = read_json(&root.join("package.json")).unwrap_or(Value::Null);
    let declared = match manifest.get("workspaces") {
        Some(Value::Array(items)) => Some(items),
        Some(Value::Object(object)) => object.get("packages").and_then(Value::as_array),
        _ => None,
    };
    if let Some(items) = declared {
        patterns.extend(items.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    if let Ok(pnpm) = fs::read_to_string(root.join("pnpm-workspace.yaml")) {
        let mut in_packages = false;
        for line in pnpm.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("packages:") {
                in_packages = true;
                continue;
            }
            if in_packages {
                if let Some(item) = trimmed.strip_prefix("- ") {
                    patterns.push(item.trim_matches(|c| c == '"' || c == '\'').to_owned());
                } else if !trimmed.is_empty() && !trimmed.starts_with('#') {
                    in_packages = false;
                }
            }
        }
    }
    let mut found = Vec::new();
    for pattern in patterns {
        let pattern = pattern.trim_start_matches("./").trim_end_matches('/');
        if pattern.starts_with('!') || pattern.contains("**") {
            continue;
        }
        let (parent, wildcard) = match pattern.strip_suffix("/*") {
            Some(parent) => (parent, true),
            None => (pattern, false),
        };
        if parent.is_empty()
            || parent
                .split('/')
                .any(|segment| segment.is_empty() || segment == ".." || segment.contains('*'))
        {
            continue;
        }
        // Joined segment by segment: a declared `libs/core` is then written
        // as the directories listed beside it are, on Windows too.
        let base = parent
            .split('/')
            .fold(root.to_path_buf(), |path, segment| path.join(segment));
        let candidates: Vec<PathBuf> = if wildcard {
            match fs::read_dir(&base) {
                Ok(entries) => entries
                    .filter_map(Result::ok)
                    .map(|entry| entry.path())
                    .collect(),
                Err(_) => Vec::new(),
            }
        } else {
            vec![base]
        };
        for candidate in candidates {
            let is_dir = fs::symlink_metadata(&candidate)
                .is_ok_and(|metadata| metadata.file_type().is_dir());
            if is_dir
                && candidate.join("package.json").is_file()
                && !nested_checkout(&candidate)
                && !owned_workspace_store(&candidate)
            {
                found.push(candidate);
            }
        }
    }
    Ok(found)
}

fn string_targets(value: &Value, depth: usize, output: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    match value {
        Value::String(value) => output.push(value.clone()),
        Value::Array(values) => {
            for value in values {
                string_targets(value, depth + 1, output);
            }
        }
        Value::Object(values) => {
            for value in values.values() {
                string_targets(value, depth + 1, output);
            }
        }
        _ => {}
    }
}

fn entry_targets(directory: &Path, manifest: &Value) -> Vec<PathBuf> {
    let mut targets = Vec::new();
    for key in ["main", "module", "browser", "bin", "exports"] {
        if let Some(value) = manifest.get(key) {
            string_targets(value, 0, &mut targets);
        }
    }
    targets
        .into_iter()
        .filter_map(|target| {
            if !target.starts_with('.') || target.contains("node_modules") {
                return None;
            }
            let prefix = target
                .split('*')
                .next()
                .unwrap_or_default()
                .trim_end_matches('/');
            (!prefix.is_empty()).then(|| resolve(directory, prefix))
        })
        .collect()
}

pub(crate) fn strip_jsonc_comments(contents: &str) -> String {
    let mut output = String::with_capacity(contents.len());
    let mut chars = contents.chars().peekable();
    let mut string = false;
    let mut escaped = false;
    while let Some(character) = chars.next() {
        if string {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                string = false;
            }
            continue;
        }
        if character == '"' {
            string = true;
            output.push(character);
        } else if character == '/' && chars.peek() == Some(&'/') {
            chars.next();
            for comment in chars.by_ref() {
                if comment == '\n' {
                    output.push('\n');
                    break;
                }
            }
        } else if character == '/' && chars.peek() == Some(&'*') {
            chars.next();
            let mut previous = '\0';
            for comment in chars.by_ref() {
                if previous == '*' && comment == '/' {
                    break;
                }
                previous = comment;
            }
        } else {
            output.push(character);
        }
    }
    output
}

pub(crate) fn strip_trailing_commas(contents: &str) -> String {
    let chars = contents.chars().collect::<Vec<_>>();
    let mut output = String::with_capacity(contents.len());
    let mut string = false;
    let mut escaped = false;
    for (index, character) in chars.iter().copied().enumerate() {
        if string {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                string = false;
            }
            continue;
        }
        if character == '"' {
            string = true;
            output.push(character);
        } else if character == ','
            && chars[index + 1..]
                .iter()
                .find(|character| !character.is_whitespace())
                .is_some_and(|character| matches!(character, '}' | ']'))
        {
        } else {
            output.push(character);
        }
    }
    output
}

fn tsconfig_roots(directory: &Path) -> Vec<PathBuf> {
    let path = directory.join("tsconfig.json");
    let Some(contents) = fs::read_to_string(path).ok() else {
        return Vec::new();
    };
    let jsonc = strip_trailing_commas(&strip_jsonc_comments(&contents));
    let Some(config) = serde_json::from_str::<Value>(&jsonc).ok() else {
        return Vec::new();
    };
    let mut values = Vec::new();
    if let Some(root_dir) = config
        .get("compilerOptions")
        .and_then(|options| options.get("rootDir"))
        .and_then(Value::as_str)
    {
        values.push(root_dir.to_owned());
    }
    if let Some(include) = config.get("include").and_then(Value::as_array) {
        values.extend(include.iter().filter_map(Value::as_str).map(str::to_owned));
    }
    if values.is_empty() {
        return vec![directory.to_owned()];
    }
    values
        .into_iter()
        .filter_map(|value| {
            if value.starts_with('!') {
                return None;
            }
            let prefix = value
                .find(['?', '*', '{', '['])
                .map_or(value.as_str(), |index| &value[..index])
                .trim_end_matches('/');
            // `next-env.d.ts` is in every Next.js `include`; a declaration
            // holds nothing to measure and is not a root.
            (!prefix.is_empty() && !declaration_file(prefix)).then(|| resolve(directory, prefix))
        })
        .collect()
}

fn within(parent: &Path, child: &Path) -> bool {
    child == parent || child.starts_with(parent)
}

fn nearest_package_root<'a>(path: &Path, packages: &'a [PathBuf]) -> Option<&'a PathBuf> {
    packages
        .iter()
        .filter(|directory| within(directory, path))
        .max_by_key(|directory| directory.components().count())
}

/// Reason for a file under no root that first-party source loads.
pub const LOADED_REASON: &str = "loaded by first-party source";

/// A file under no source root that first-party source imports is code the
/// product runs: a `shared/` folder beside `src/`. It is followed from what
/// is known to be source, before anything runs, so nobody has to name a root
/// for a project whose tests pass without Supercov. What only tests load
/// stays ambiguous: tests load their own helpers, mocks and simulators, and
/// counting those would measure test code as product.
fn promote_loaded(
    root: &Path,
    entries: &mut [SourceScopeEntry],
    included: &mut Vec<PathBuf>,
    limitations: &mut Vec<SourceLimitation>,
) {
    if !entries
        .iter()
        .any(|entry| entry.status == SourceScopeStatus::Ambiguous)
    {
        return;
    }
    let starts = entries
        .iter()
        .filter(|entry| entry.status == SourceScopeStatus::Included)
        .map(|entry| entry.file.clone())
        .collect::<Vec<_>>();
    let loaded = crate::test_loads::Imports::new(root)
        .loads(starts.iter().map(String::as_str))
        .files;
    for entry in entries.iter_mut().filter(|entry| {
        entry.status == SourceScopeStatus::Ambiguous && loaded.contains(&entry.file)
    }) {
        entry.status = SourceScopeStatus::Included;
        entry.reason = LOADED_REASON.into();
        included.push(root.join(&entry.file));
        limitations.retain(|limitation| limitation.file != entry.file);
    }
    included.sort();
}

fn scope_limitation(file: &str) -> SourceLimitation {
    let digest = Sha256::digest(file.as_bytes());
    let id = digest[..10]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    SourceLimitation {
        id: format!("scope:{id}"),
        kind: "source-scope".into(),
        file: file.into(),
        line: 1,
        column: 1,
        source: file.into(),
        reason: "JavaScript/TypeScript under no source root that no first-party source imports, so the run did not measure it. If it is your code, name the roots with SUPERCOV_SOURCE_ROOTS.".into(),
    }
}

// A declared Jest setup file is test infrastructure even when it lives under
// a source root with an arbitrary name. Instrumenting its mock factories adds
// out-of-scope bindings that Babel's jest-hoist correctly rejects.
fn declared_test_setup(packages: &[PathBuf]) -> BTreeSet<PathBuf> {
    let mut paths = BTreeSet::new();
    for package in packages {
        let manifest = read_json(&package.join("package.json")).unwrap_or(Value::Null);
        let Some(jest) = manifest.get("jest") else {
            continue;
        };
        let root = jest
            .get("rootDir")
            .and_then(Value::as_str)
            .map(|value| {
                resolve(
                    package,
                    value.replace("<rootDir>", &package.to_string_lossy()),
                )
            })
            .unwrap_or_else(|| package.clone());
        for field in [
            "setupFiles",
            "setupFilesAfterEnv",
            "globalSetup",
            "globalTeardown",
        ] {
            let values = match jest.get(field) {
                Some(Value::Array(values)) => values.iter().collect::<Vec<_>>(),
                Some(value) => vec![value],
                None => Vec::new(),
            };
            for value in values.into_iter().filter_map(Value::as_str) {
                let path = resolve(&root, value.replace("<rootDir>", &root.to_string_lossy()));
                if path.is_file() {
                    paths.insert(path);
                }
            }
        }
    }
    paths
}

pub fn discover_source_scope(
    root: &Path,
    configured_roots: Option<&[String]>,
) -> Result<DiscoveredSourceScope, SourceDiscoveryError> {
    let root = lexical_normalize(root);
    let root_metadata = fs::symlink_metadata(&root).map_err(|error| io_error(&root, error))?;
    if !root_metadata.file_type().is_dir() {
        return Err(SourceDiscoveryError::InvalidRoot(root));
    }
    let packages = package_directories(&root)?;
    let test_setup = declared_test_setup(&packages);
    let explicit = configured_roots.is_some_and(|roots| !roots.is_empty());
    let include_roots = if explicit {
        configured_roots
            .unwrap_or_default()
            .iter()
            .map(|directory| resolve(&root, directory))
            .collect::<Vec<_>>()
    } else {
        packages
            .iter()
            .flat_map(|directory| {
                let manifest = read_json(&directory.join("package.json")).unwrap_or(Value::Null);
                let next_files = depends_on_next(&manifest)
                    .then_some(NEXT_ROOT_FILES)
                    .into_iter()
                    .flatten()
                    .flat_map(|name| {
                        ["ts", "tsx", "js", "jsx", "mjs"]
                            .map(|extension| directory.join(format!("{name}.{extension}")))
                    });
                let candidates = SOURCE_DIRECTORIES
                    .iter()
                    .map(|name| directory.join(name))
                    .chain(next_files)
                    .chain(entry_targets(directory, &manifest))
                    .chain(tsconfig_roots(directory))
                    // A tsconfig that includes a sibling package
                    // (`../packages/ui/src`) names code outside this
                    // project: not its source, and not in its copy. Only
                    // a root the user names is held to being inside.
                    .filter(|candidate| within(&root, candidate))
                    .collect::<Vec<_>>();
                // A declared package that keeps its code somewhere
                // unconventional (a Shopify theme extension's `frontend/` and
                // `blocks/`, say) is still first-party source. Its own
                // directory becomes the root; generated subtrees stay excluded
                // by the walker as everywhere else.
                if directory != &root
                    && !candidates
                        .iter()
                        .any(|candidate| fs::symlink_metadata(candidate).is_ok())
                {
                    return vec![directory.clone()];
                }
                candidates
            })
            .collect()
    };
    let existing_roots = existing_roots(include_roots, explicit)?;
    let app_directories = packages
        .iter()
        .filter(|directory| {
            depends_on_next(&read_json(&directory.join("package.json")).unwrap_or(Value::Null))
        })
        .flat_map(|directory| [directory.join("app"), directory.join("src/app")])
        .collect::<Vec<_>>();
    let mut all_files = Vec::new();
    files_under(&root, &root, &mut all_files)?;
    all_files.sort();

    let mut entries = Vec::new();
    let mut included = Vec::new();
    let mut limitations = Vec::new();
    for path in all_files {
        let file = walked(&root, &path);
        let package_root = nearest_package_root(&path, &packages)
            .map(|package| walked(&root, package))
            .filter(|package| package != ".");
        let entry = |status, reason: &str| SourceScopeEntry {
            file: file.clone(),
            status,
            reason: reason.into(),
            package_root: package_root.clone(),
        };
        let named = explicit
            .then(|| below_named_root(&existing_roots, &path))
            .flatten();
        let guessed = match named {
            Some(local) => local.to_string_lossy().replace('\\', "/"),
            None => file.clone(),
        };
        if declaration_file(&file) {
            entries.push(entry(SourceScopeStatus::Excluded, "TypeScript declaration"));
        } else if test_setup.contains(&path) {
            entries.push(entry(SourceScopeStatus::Excluded, "declared test setup"));
        } else if test_or_fixture(&guessed) && !next_app_file(&app_directories, &path) {
            entries.push(entry(SourceScopeStatus::Excluded, "test or fixture source"));
        } else if tool_script(&guessed) {
            entries.push(entry(
                SourceScopeStatus::Excluded,
                "conventional tool script",
            ));
        } else if benchmark(&guessed) {
            entries.push(entry(SourceScopeStatus::Excluded, "benchmark"));
        } else if example(&file, package_root.as_deref()) {
            entries.push(entry(SourceScopeStatus::Excluded, "example"));
        } else if config_file(&file) {
            entries.push(entry(
                SourceScopeStatus::Excluded,
                "build/test/tool configuration",
            ));
        } else if built_asset(&file) {
            entries.push(entry(SourceScopeStatus::Excluded, BUILT_ASSET_REASON));
        } else if root_contains(&existing_roots, &path) {
            included.push(path);
            entries.push(entry(
                SourceScopeStatus::Included,
                if explicit {
                    "explicit source root"
                } else {
                    "discovered package source root"
                },
            ));
        } else if explicit {
            entries.push(entry(
                SourceScopeStatus::Excluded,
                "outside explicit source roots",
            ));
        } else {
            entries.push(entry(
                SourceScopeStatus::Ambiguous,
                "unclassified first-party source",
            ));
            limitations.push(scope_limitation(&file));
        }
    }
    promote_loaded(&root, &mut entries, &mut included, &mut limitations);
    let source_files = included
        .iter()
        .map(|path| walked(&root, path))
        .collect::<Vec<_>>();
    let source_roots = existing_roots
        .iter()
        .map(|path| local_path(&root, path))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(DiscoveredSourceScope {
        source_files,
        source_roots: source_roots.clone(),
        scope: SourceScope {
            version: 1,
            mode: if explicit {
                SourceScopeMode::Explicit
            } else {
                SourceScopeMode::Automatic
            },
            roots: source_roots,
            entries,
        },
        limitations,
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;

    /// A directory a walk cannot list, or look at, or an entry it cannot
    /// read, fails discovery; a directory it may not enter, where the system
    /// enforces that, fails the source walk.
    #[test]
    fn a_walk_that_cannot_read_what_it_walks_fails() {
        let root = std::env::temp_dir().join(format!(
            "supercov-discovery-walk-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.js"), "export const a = 1;\n").unwrap();
        let mut found = Vec::new();
        files_under(&root, &root.join("src"), &mut found).unwrap();
        assert_eq!(found, [root.join("src/a.js")]);
        for fault in ["fs.read_dir", "fs.read_dir entry"] {
            assert!(
                crate::fault::during(fault, || read_directory(&root)).is_err(),
                "{fault}"
            );
        }
        assert!(
            crate::fault::during("fs.symlink_metadata", || {
                files_under(&root, &root.join("src"), &mut Vec::new())
            })
            .is_err()
        );
        #[cfg(unix)]
        if unsafe { libc::geteuid() } != 0 {
            use std::os::unix::fs::PermissionsExt;
            let closed = root.join("src/closed");
            fs::create_dir(&closed).unwrap();
            fs::set_permissions(&closed, fs::Permissions::from_mode(0o000)).unwrap();
            assert!(files_under(&root, &root.join("src"), &mut Vec::new()).is_err());
            fs::set_permissions(&closed, fs::Permissions::from_mode(0o755)).unwrap();
        }
        fs::remove_dir_all(root).unwrap();
    }

    /// Text with nothing to strip, a comment that runs to the end, and
    /// targets of every JSON kind, each read as written.
    #[test]
    fn jsonc_and_targets_at_their_edges() {
        assert_eq!(strip_jsonc_comments(""), "");
        assert_eq!(strip_trailing_commas(""), "");
        assert!(!strip_jsonc_comments("{} //").contains("//"));
        assert!(!strip_jsonc_comments("{} /* open").contains("/*"));
        // A comment opened at the very end, and the empty path.
        assert_eq!(strip_jsonc_comments("{} /*"), "{} ");
        assert_eq!(lexical_normalize(Path::new("")), PathBuf::new());
        let mut targets = Vec::new();
        string_targets(
            &serde_json::json!({"a": [], "b": {}, "c": 5, "d": ["./x.js", {"e": "./y.js"}]}),
            0,
            &mut targets,
        );
        assert_eq!(targets, ["./x.js", "./y.js"]);
    }

    /// No package declares no setup; a Jest setup named by one string under
    /// a `rootDir` is test infrastructure; `.` in a path is no step.
    #[test]
    fn a_jest_setup_is_read_from_any_form_of_its_field() {
        assert!(declared_test_setup(&[]).is_empty());
        assert_eq!(lexical_normalize(Path::new("/a/./b")), Path::new("/a/b"));
        let root = std::env::temp_dir().join(format!(
            "supercov-jest-setup-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(root.join("config")).unwrap();
        fs::write(root.join("config/setup.js"), "").unwrap();
        fs::write(
            root.join("package.json"),
            r#"{"jest": {"rootDir": "<rootDir>/config", "globalSetup": "<rootDir>/setup.js", "setupFiles": 5}}"#,
        )
        .unwrap();
        let setup = declared_test_setup(std::slice::from_ref(&root));
        assert_eq!(setup.len(), 1, "{setup:?}");
        fs::remove_dir_all(root).unwrap();
    }

    /// A project the repository around it ignores is ignored, unless a
    /// deeper rule takes it back.
    #[test]
    fn an_enclosing_repository_ignores_a_project_until_a_deeper_rule_keeps_it() {
        let repository = std::env::temp_dir().join(format!(
            "supercov-enclosing-{}-{}",
            std::process::id(),
            line!()
        ));
        let project = repository.join("tmp/copy");
        fs::create_dir_all(repository.join(".git")).unwrap();
        fs::create_dir_all(&project).unwrap();
        fs::write(repository.join(".gitignore"), "tmp/\n").unwrap();
        assert!(ignored_by_enclosing_repository(&project));
        fs::write(repository.join("tmp/.gitignore"), "!copy/\n").unwrap();
        assert!(!ignored_by_enclosing_repository(&project));
        fs::remove_dir_all(repository).unwrap();
    }

    /// An empty pnpm workspace declares no package.
    #[test]
    fn an_empty_pnpm_workspace_declares_nothing() {
        let root = std::env::temp_dir().join(format!(
            "supercov-empty-pnpm-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("pnpm-workspace.yaml"), "").unwrap();
        fs::write(root.join("package.json"), r#"{"workspaces": 5}"#).unwrap();
        assert!(declared_workspace_packages(&root).unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// A tsconfig that is not there, or no JSON, names no root.
    #[test]
    fn a_tsconfig_that_is_no_json_names_no_root() {
        let root = std::env::temp_dir().join(format!(
            "supercov-tsconfig-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(&root).unwrap();
        assert!(tsconfig_roots(&root).is_empty());
        fs::write(root.join("tsconfig.json"), "{").unwrap();
        assert!(tsconfig_roots(&root).is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    /// A path whose name is not UTF-8 is named, not read as some other.
    #[cfg(unix)]
    #[test]
    fn a_path_that_is_not_utf8_is_refused_by_name() {
        use std::os::unix::ffi::OsStrExt;
        let root = std::path::Path::new("/root");
        let path = root.join(std::ffi::OsStr::from_bytes(b"\xff.js"));
        assert!(matches!(
            local_path(root, &path),
            Err(SourceDiscoveryError::NonUtf8Path(named)) if named == path
        ));
    }

    /// A source or package directory whose name is not UTF-8 is refused by
    /// name rather than read as some other. (macOS names none.)
    #[cfg(target_os = "linux")]
    #[test]
    fn a_tree_holding_a_name_that_is_not_utf8_is_refused_by_name() {
        use std::os::unix::ffi::OsStrExt;
        let root = std::env::temp_dir().join(format!(
            "supercov-non-utf8-tree-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("package.json"), "{}").unwrap();
        fs::write(root.join("src/a.js"), "").unwrap();
        let odd = std::ffi::OsStr::from_bytes(b"\xff");
        // Deeper than packages are looked for, so the source walk meets it.
        let deep = root.join("src/a/b/c/d/e/f");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join(odd).with_extension("js"), "").unwrap();
        assert!(matches!(
            discover_source_scope(&root, None),
            Err(SourceDiscoveryError::NonUtf8Path(_))
        ));
        fs::remove_dir_all(root.join("src/a")).unwrap();
        fs::create_dir(root.join(odd)).unwrap();
        assert!(matches!(
            discover_source_scope(&root, None),
            Err(SourceDiscoveryError::NonUtf8Path(_))
        ));
        fs::remove_dir_all(root).unwrap();
    }

    /// A bundler's output is a script under an asset directory with an
    /// eight-character hash that has a digit or both cases.
    #[test]
    fn built_assets_are_known_by_directory_and_hash() {
        for (file, built) in [
            ("dist/assets/index-BxY3kQ9a.js", true),
            ("public/app-AbCdEfGh.mjs", true),
            ("static/x-ab_c-d9e.cjs", true),
            ("public/app-abcdefgh.js", false),
            ("public/app-ab.cd_e9.js", false),
            ("src/app-Ab3dEfGh.js", false),
            ("app-Ab3dEfGh.js", false),
            ("static/a.css", false),
            ("static/short.js", false),
            ("static/abcdefghij.js", false),
            // Capitals alone are a word, not a hash.
            ("dist/assets/x-ABCDEFGH.js", false),
        ] {
            assert_eq!(built_asset(file), built, "{file}");
        }
        for (path, normal) in [
            ("a/./b/../c", "a/c"),
            // Only a leading `.` is a component of its own.
            ("./a", "a"),
            ("../a", "../a"),
            ("a/../../b", "../b"),
        ] {
            assert_eq!(
                lexical_normalize(Path::new(path)),
                Path::new(normal),
                "{path}"
            );
        }
    }

    #[test]
    fn a_file_under_no_root_that_source_loads_is_source_and_one_only_a_test_loads_is_not() {
        let root = repository(
            "loaded",
            &[
                ("package.json", r#"{"name":"loaded","type":"module"}"#),
                (
                    "src/a.js",
                    "import { y } from '../shared/y.js';\nexport const a = y;\n",
                ),
                (
                    "shared/y.js",
                    "import { deep } from './deep';\nexport const y = deep;\n",
                ),
                ("shared/deep.ts", "export const deep = 1;\n"),
                ("docs/x.js", "export const x = 1;\n"),
                ("extra/z.js", "export const z = 1;\n"),
                (
                    "test/a.test.js",
                    "import '../src/a.js';\nimport { x } from '../docs/x.js';\n",
                ),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        // Through source, and through what that loads in turn.
        for file in ["shared/y.js", "shared/deep.ts"] {
            let found = entry(&discovered, file);
            assert_eq!(found.status, SourceScopeStatus::Included, "{file}");
            assert_eq!(found.reason, LOADED_REASON, "{file}");
            assert!(discovered.source_files.contains(&file.to_owned()), "{file}");
        }
        // Only a test loads one, nothing loads the other: still questions,
        // and still said.
        for file in ["docs/x.js", "extra/z.js"] {
            assert_eq!(
                entry(&discovered, file).status,
                SourceScopeStatus::Ambiguous,
                "{file}"
            );
        }
        assert_eq!(
            discovered
                .limitations
                .iter()
                .map(|limitation| limitation.file.as_str())
                .collect::<Vec<_>>(),
            ["docs/x.js", "extra/z.js"]
        );
        // Named roots settle it: nothing is promoted past them.
        let named = discover_source_scope(&root, Some(&["src".to_owned()])).unwrap();
        assert_eq!(
            entry(&named, "docs/x.js").status,
            SourceScopeStatus::Excluded
        );
        fs::remove_dir_all(root).unwrap();
    }

    fn repository(label: &str, files: &[(&str, &str)]) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-source-{label}-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir_all(&root).unwrap();
        for (file, contents) in files {
            let path = root.join(file);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, contents).unwrap();
        }
        root
    }

    #[test]
    fn only_a_directory_has_source_files_under_it() {
        let root = repository("files-under-file", &[("main.ts", "")]);
        assert!(matches!(
            files_under(&root, &root.join("main.ts"), &mut Vec::new()),
            Err(SourceDiscoveryError::InvalidRoot(path)) if path == root.join("main.ts")
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn task_runner_files_are_tooling_and_comments_end_where_json_says() {
        for file in [
            "gulpfile.js",
            "gruntfile.js",
            "Gulpfile.mjs",
            "vite.config.ts",
        ] {
            assert!(config_file(file), "{file}");
        }
        assert!(!config_file("gulp.js"));
        // A config that is not JavaScript is not a config file of the source.
        assert!(!config_file("vite.config.json"));
        // A slash that opens no comment, and a star inside one, are text.
        assert_eq!(
            strip_jsonc_comments("{\"a\": 1 /* x * y */, \"b\": 2 / 1 // z\n}"),
            "{\"a\": 1 , \"b\": 2 / 1 \n}"
        );
    }

    /// What a package names as its entries, its include patterns and its
    /// test setup are read as far as they reach real files of its own.
    #[test]
    fn declarations_are_followed_only_to_the_projects_own_files() {
        let root = repository(
            "declared",
            &[
                (
                    "package.json",
                    r#"{"name":"declared","workspaces":["packages/*"],
                        "exports":{"a":{"b":{"c":{"d":{"e":{"f":{"g":{"h":{"i":{"j":"./deep/x.js"}}}}}}}}}},
                        "main":"./node_modules/dep/index.js",
                        "jest":{"setupFiles":["<rootDir>/missing.js","<rootDir>/setup.js"]}}"#,
                ),
                ("setup.js", "export const s = 1;\n"),
                ("deep/x.js", "export const x = 1;\n"),
                ("tsconfig.json", r#"{"include":["src/**/*","!src/legacy"]}"#),
                ("src/a.js", "export const a = 1;\n"),
                ("packages/ui/package.json", r#"{"name":"ui"}"#),
                ("packages/ui/index.js", "export const ui = 1;\n"),
                ("packages/vendored/package.json", r#"{"name":"vendored"}"#),
                ("packages/vendored/.git/HEAD", "ref: refs/heads/main\n"),
                ("packages/vendored/index.js", "export const v = 1;\n"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        let packages = package_directories(&root).unwrap();
        assert!(packages.contains(&root.join("packages/ui")), "{packages:?}");
        // A package that is a checkout of its own is not this project's.
        assert!(
            !packages.contains(&root.join("packages/vendored")),
            "{packages:?}"
        );
        // Only setup files that exist are setup.
        assert_eq!(
            declared_test_setup(std::slice::from_ref(&root)),
            BTreeSet::from([root.join("setup.js")])
        );
        // An entry deeper than anything a manifest is read to is not a root.
        assert!(
            discovered
                .scope
                .entries
                .iter()
                .all(|e| e.file != "deep/x.js")
                || entry(&discovered, "deep/x.js").reason != "package entry",
            "{:?}",
            discovered.scope.entries
        );
        fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    fn a_root_that_is_no_directory_or_no_source_is_refused_by_name() {
        let root = repository(
            "refused",
            &[
                ("src/a.js", "export const a = 1;\n"),
                ("docs/README.md", "# docs\n"),
            ],
        );
        // A project root that is a file.
        assert!(matches!(
            discover_source_scope(&root.join("src/a.js"), None),
            Err(SourceDiscoveryError::InvalidRoot(_))
        ));
        // A root below a file is neither missing nor a root: the error says
        // so. Windows answers such a path as not found, so there it is a
        // root that does not exist, refused as one later.
        let below_a_file = discover_source_scope(&root, Some(&["src/a.js/inner".into()]));
        assert_eq!(below_a_file.is_err(), cfg!(unix));
        // A root that exists but holds no source is not called missing.
        let environment = BTreeMap::from([(SOURCE_ROOTS_VARIABLE.to_owned(), "docs".to_owned())]);
        let roots = ExplicitSourceRoots::from_environment(&root, &environment)
            .unwrap()
            .unwrap();
        let refused = roots.refuse_if_empty(0, "JavaScript").unwrap_err();
        assert!(refused.contains("left no JavaScript source"), "{refused}");
        assert!(!refused.contains("does not exist"), "{refused}");
        // Relative roots name the same file in a copy as in the project.
        let copy = repository("refused-copy", &[("docs/README.md", "# docs\n")]);
        let rebased = ExplicitSourceRoots::from_environment_in(&root, &copy, &environment)
            .unwrap()
            .unwrap();
        assert!(
            rebased
                .refuse_if_empty(0, "Go")
                .unwrap_err()
                .contains("docs")
        );
        fs::remove_dir_all(&root).unwrap();
        fs::remove_dir_all(&copy).unwrap();
    }

    fn entry<'a>(scope: &'a DiscoveredSourceScope, file: &str) -> &'a SourceScopeEntry {
        scope
            .scope
            .entries
            .iter()
            .find(|entry| entry.file == file)
            .unwrap()
    }

    #[test]
    fn a_file_the_project_tells_git_to_ignore_is_not_its_source() {
        // Actual's build writes a 5 MB worker bundle under
        // packages/loot-core/lib-dist, which its own nested .gitignore names.
        let files = [
            ("package.json", r#"{"workspaces":["packages/*"]}"#),
            ("packages/core/package.json", r#"{"name":"core"}"#),
            ("packages/core/.gitignore", "lib-dist\n"),
            ("packages/core/src/index.ts", "export const a = 1;"),
            (
                "packages/core/lib-dist/browser/worker.dev.js",
                "var bundled = 1;",
            ),
        ];
        let repository_root = repository("gitignored", &files);
        fs::create_dir_all(repository_root.join(".git")).unwrap();
        let discovered = discover_source_scope(&repository_root, None).unwrap();
        assert!(
            discovered
                .source_files
                .contains(&"packages/core/src/index.ts".into())
        );
        assert!(
            !discovered
                .source_files
                .iter()
                .any(|file| file.contains("lib-dist")),
            "{:?}",
            discovered.source_files
        );
        fs::remove_dir_all(repository_root).unwrap();

        // Outside a git repository a .gitignore means nothing.
        let plain = repository("not-a-repository", &files);
        let discovered = discover_source_scope(&plain, None).unwrap();
        assert!(
            discovered
                .scope
                .entries
                .iter()
                .any(|entry| entry.file.contains("lib-dist"))
        );
        fs::remove_dir_all(plain).unwrap();
    }

    #[test]
    fn a_project_kept_where_an_enclosing_repository_ignores_it_keeps_its_source() {
        let files = [
            // What Supercov's own store writes, so nothing inside is tracked.
            ("scratch/.gitignore", "*\n"),
            ("scratch/app/package.json", r#"{"name":"app"}"#),
            ("scratch/app/.gitignore", "dist\n"),
            ("scratch/app/src/index.js", "export const a = 1;"),
            ("scratch/app/dist/bundle.js", "var bundled = 1;"),
        ];
        let repository_root = repository("enclosing-ignore", &files);
        fs::create_dir_all(repository_root.join(".git")).unwrap();
        let discovered = discover_source_scope(&repository_root.join("scratch/app"), None).unwrap();
        assert_eq!(discovered.source_files, ["src/index.js"]);

        // A project the enclosing repository tracks still follows its rules.
        fs::remove_file(repository_root.join("scratch/.gitignore")).unwrap();
        fs::write(repository_root.join(".gitignore"), "*.gen.js\n").unwrap();
        fs::write(repository_root.join("scratch/app/src/api.gen.js"), "var a;").unwrap();
        let discovered = discover_source_scope(&repository_root.join("scratch/app"), None).unwrap();
        assert_eq!(discovered.source_files, ["src/index.js"]);
        fs::remove_dir_all(repository_root).unwrap();
    }

    #[test]
    fn excludes_declared_jest_setup_without_hiding_neighboring_application_code() {
        let root = repository(
            "jest-setup",
            &[
                (
                    "package.json",
                    r#"{"jest":{"setupFilesAfterEnv":["<rootDir>/src/bootstrap.js"]}}"#,
                ),
                (
                    "src/bootstrap.js",
                    "jest.mock('storage', () => ({read: () => null}));",
                ),
                ("src/index.js", "export const value = 1;"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(discovered.source_files, ["src/index.js"]);
        assert_eq!(
            entry(&discovered, "src/bootstrap.js").reason,
            "declared test setup"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn example_apps_and_conf_files_are_not_source() {
        // uuid: a tsconfig with no `include` makes the whole checkout a root.
        let root = repository(
            "examples",
            &[
                ("package.json", r#"{"exports":{".":"./dist/index.js"}}"#),
                ("tsconfig.json", r#"{"exclude":["dist","examples"]}"#),
                ("src/v4.ts", "export const v4 = () => ''"),
                ("wdio.conf.js", "exports.config = {}"),
                ("karma.conf.cjs", "module.exports = () => {}"),
                ("examples/browser-esmodules/package.json", "{}"),
                ("examples/browser-esmodules/example.js", "console.log(1)"),
                ("examples/utils/testpage.js", "export default 1"),
                ("example/demo.js", "console.log(1)"),
                ("app/examples/page.tsx", "export default function Page() {}"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            discovered.source_files,
            ["app/examples/page.tsx", "src/v4.ts"]
        );
        for file in [
            "examples/browser-esmodules/example.js",
            "examples/utils/testpage.js",
            "example/demo.js",
        ] {
            assert_eq!(entry(&discovered, file).reason, "example", "{file}");
        }
        for file in ["wdio.conf.js", "karma.conf.cjs"] {
            assert_eq!(
                entry(&discovered, file).reason,
                "build/test/tool configuration",
                "{file}"
            );
        }
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn discovers_conventional_and_workspace_sources_and_blocks_ambiguity() {
        let root = repository(
            "automatic",
            &[
                ("package.json", r#"{"workspaces":["packages/*"]}"#),
                ("src/index.ts", "export const root = true"),
                ("lib/helper.js", "export const helper = true"),
                ("src/index.test.ts", "test('root', () => {})"),
                ("tests/e2e.spec.ts", "test('e2e', () => {})"),
                ("scripts/release.mjs", "export const release = true"),
                ("vite.config.ts", "export default {}"),
                ("build.mjs", "export default async function build() {}"),
                (".eslintrc.cjs", "module.exports = {}"),
                (".graphqlrc.ts", "export default {}"),
                ("orphan.ts", "export const missed = true"),
                ("packages/ui/package.json", r#"{"module":"./src/index.ts"}"#),
                ("packages/ui/src/index.ts", "export const ui = true"),
                ("packages/ui/tests/ui.spec.ts", "test('ui', () => {})"),
                ("dist/generated.js", "generated"),
                (".cache/tool/generated.js", "cached"),
                ("src/parse.bench.ts", "bench('parse', () => {})"),
                (
                    "benchmarks/value-slice.bench.ts",
                    "bench('slice', () => {})",
                ),
                ("src/benchmarkReport.ts", "export const report = 1"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            discovered.source_files,
            [
                "lib/helper.js",
                "packages/ui/src/index.ts",
                "src/benchmarkReport.ts",
                "src/index.ts"
            ]
        );
        for file in ["src/parse.bench.ts", "benchmarks/value-slice.bench.ts"] {
            assert_eq!(entry(&discovered, file).reason, "benchmark", "{file}");
        }
        assert_eq!(
            entry(&discovered, "orphan.ts").status,
            SourceScopeStatus::Ambiguous
        );
        assert_eq!(
            entry(&discovered, "scripts/release.mjs").reason,
            "conventional tool script"
        );
        assert_eq!(
            entry(&discovered, "build.mjs").reason,
            "build/test/tool configuration"
        );
        assert_eq!(
            entry(&discovered, "packages/ui/src/index.ts").package_root,
            Some("packages/ui".into())
        );
        assert_eq!(discovered.limitations.len(), 1);
        assert_eq!(discovered.limitations[0].file, "orphan.ts");
        assert_eq!(discovered.limitations[0].id.len(), "scope:".len() + 20);
        assert!(
            discovered
                .scope
                .entries
                .iter()
                .all(|entry| !entry.file.contains(".cache"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn declared_workspaces_outside_conventional_parents_are_package_roots() {
        let root = repository(
            "declared-workspaces",
            &[
                ("package.json", r#"{"workspaces":["app_extensions/*"]}"#),
                ("app/main.ts", "product"),
                ("app_extensions/discounts/package.json", "{}"),
                ("app_extensions/discounts/src/index.ts", "extension"),
                // No conventional source directory at all: the package itself
                // is the root, so its frontend code is still first-party.
                ("app_extensions/upsells/package.json", "{}"),
                ("app_extensions/upsells/frontend/embed.ts", "embed"),
                ("app_extensions/upsells/dist/embed.js", "built"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            discovered.source_files,
            [
                "app/main.ts",
                "app_extensions/discounts/src/index.ts",
                "app_extensions/upsells/frontend/embed.ts",
            ]
        );
        assert!(
            discovered.limitations.is_empty(),
            "declared packages must not be blockers: {:?}",
            discovered.limitations
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn hashed_bundles_in_asset_directories_are_built_assets() {
        let root = repository(
            "built-assets",
            &[
                ("package.json", r#"{"workspaces":["app_extensions/*"]}"#),
                ("app/main.ts", "product"),
                ("app_extensions/upsells/package.json", "{}"),
                ("app_extensions/upsells/frontend/embed.ts", "source"),
                (
                    "app_extensions/upsells/assets/app-embed-Be-aUw9g.js",
                    "bundle",
                ),
                ("app_extensions/upsells/assets/stylex-DAnmLURx.js", "bundle"),
                // A hand-written helper in assets keeps its ordinary treatment.
                (
                    "app_extensions/upsells/assets/theme-helper.js",
                    "hand written",
                ),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert!(
            !discovered
                .source_files
                .iter()
                .any(|file| file.contains("-Be-aUw9g.js") || file.contains("-DAnmLURx.js"))
        );
        assert_eq!(
            entry(
                &discovered,
                "app_extensions/upsells/assets/app-embed-Be-aUw9g.js"
            )
            .reason,
            "built asset"
        );
        assert!(
            discovered.limitations.is_empty(),
            "bundles are not blockers: {:?}",
            discovered.limitations
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn root_level_hidden_directories_are_tooling_not_source() {
        let root = repository(
            "root-hidden",
            &[
                ("package.json", "{}"),
                ("app/main.ts", "product"),
                (".shopify/bundle/upsells/frontend/embed.js", "cli bundle"),
                (".vercel/output/functions/index.js", "deploy output"),
                // A nested hidden directory inside a source root keeps its
                // ordinary treatment.
                ("app/.generated/schema.ts", "generated types"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            discovered.source_files,
            ["app/.generated/schema.ts", "app/main.ts"]
        );
        assert!(
            discovered.limitations.is_empty(),
            "{:?}",
            discovered.limitations
        );
        assert!(discovered.scope.entries.iter().all(
            |entry| !entry.file.starts_with(".shopify/") && !entry.file.starts_with(".vercel/")
        ));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn nested_checkouts_are_neither_source_nor_limitations() {
        let root = repository(
            "nested-checkout",
            &[
                ("package.json", "{}"),
                ("app/main.ts", "product"),
                // An agent worktree: a full copy of the project carrying its own
                // `.git` file. Real project, 1,032 of these turned into blockers.
                (".claude/worktrees/agent-1/.git", "gitdir: /elsewhere"),
                (".claude/worktrees/agent-1/app/main.ts", "copy"),
                ("vendor-fork/.git/HEAD", "ref: refs/heads/main"),
                ("vendor-fork/src/index.ts", "clone"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(discovered.source_files, ["app/main.ts"]);
        assert!(
            discovered.limitations.is_empty(),
            "nested checkouts must not be blocking limitations: {:?}",
            discovered.limitations
        );
        assert!(
            discovered
                .scope
                .entries
                .iter()
                .all(|entry| !entry.file.starts_with(".claude/")
                    && !entry.file.starts_with("vendor-fork/")),
            "nested checkout files must not appear in scope at all"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn roots_enter_a_runs_identity_only_when_named() {
        // Appending nothing when no roots are named is what keeps every
        // fingerprint recorded before this change valid.
        let mut unchanged = b"pytest\0-q".to_vec();
        fold_roots_into_configuration(&mut unchanged, None);
        assert_eq!(unchanged, b"pytest\0-q");
        fold_roots_into_configuration(&mut unchanged, Some(&[]));
        assert_eq!(unchanged, b"pytest\0-q");

        let mut folded = b"pytest\0-q".to_vec();
        fold_roots_into_configuration(&mut folded, Some(&["src".to_owned(), "a.py".to_owned()]));
        assert_eq!(folded, b"pytest\0-q\0SUPERCOV_SOURCE_ROOTS=src,a.py");
    }

    #[test]
    fn the_variable_is_read_the_same_way_for_every_frontend() {
        let read = |value: &str| {
            configured_source_roots(&BTreeMap::from([(
                SOURCE_ROOTS_VARIABLE.to_owned(),
                value.to_owned(),
            )]))
        };
        assert_eq!(
            read(" src , app ,"),
            Some(vec!["src".to_owned(), "app".to_owned()])
        );
        assert_eq!(
            read(" , "),
            None,
            "naming nothing leaves discovery automatic"
        );
        assert_eq!(configured_source_roots(&BTreeMap::new()), None);
    }

    #[test]
    fn a_next_application_is_measured_where_next_keeps_its_code() {
        // A stock Next.js layout reported 89 files it could not classify:
        // root `components/`, `proxy.ts` and `instrumentation.ts`. And five
        // "test connection" endpoints were left out as tests, because under
        // `app/` a directory named `test` is a URL segment.
        let root = repository(
            "next-layout",
            &[
                ("package.json", r#"{"dependencies":{"next":"16.1.1"}}"#),
                (
                    "tsconfig.json",
                    r#"{"include":["next-env.d.ts","**/*.ts",".next/types/**/*.ts"]}"#,
                ),
                ("next-env.d.ts", "declare const next: unknown;"),
                ("app/page.tsx", "page"),
                ("app/api/wordpress/test/route.ts", "route"),
                ("app/api/wordpress/test/route.test.ts", "test"),
                ("app/api/wordpress/test/helper.ts", "helper"),
                ("app/__tests__/page.tsx", "test"),
                ("components/button.tsx", "component"),
                ("pages/api/legacy.ts", "page"),
                ("proxy.ts", "proxy"),
                ("instrumentation.ts", "instrumentation"),
                ("simulators/echo.ts", "simulator"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            discovered.source_files,
            [
                "app/api/wordpress/test/route.ts",
                "app/page.tsx",
                "components/button.tsx",
                "instrumentation.ts",
                "pages/api/legacy.ts",
                "proxy.ts",
            ]
        );
        assert_eq!(
            discovered.source_roots,
            [
                "app",
                "components",
                "instrumentation.ts",
                "pages",
                "proxy.ts"
            ],
            "a declaration file is not a root"
        );
        for file in [
            "app/api/wordpress/test/route.test.ts",
            "app/api/wordpress/test/helper.ts",
            "app/__tests__/page.tsx",
        ] {
            assert_eq!(entry(&discovered, file).reason, "test or fixture source");
        }
        // Nothing says what `simulators/` is, so it is still asked about.
        assert_eq!(
            entry(&discovered, "simulators/echo.ts").status,
            SourceScopeStatus::Ambiguous
        );
        // Named roots keep the route too.
        let roots = vec!["app".into()];
        let named = discover_source_scope(&root, Some(&roots)).unwrap();
        assert_eq!(
            named.source_files,
            ["app/api/wordpress/test/route.ts", "app/page.tsx"]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_route_file_name_means_nothing_outside_a_next_application() {
        let root = repository(
            "not-next",
            &[
                ("package.json", "{}"),
                ("app/main.ts", "main"),
                ("app/test/route.ts", "a test helper"),
                ("proxy.ts", "unknown"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(discovered.source_files, ["app/main.ts"]);
        assert_eq!(
            entry(&discovered, "app/test/route.ts").reason,
            "test or fixture source"
        );
        assert_eq!(
            entry(&discovered, "proxy.ts").status,
            SourceScopeStatus::Ambiguous
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn a_named_root_settles_what_it_names_and_guesses_only_below_it() {
        // `scripts`, or a directory called `test-utils`, read as tooling and
        // tests to discovery. Naming one as a source root says otherwise, and
        // there was no way to say it: the named root stayed excluded.
        let root = repository(
            "named-roots",
            &[
                ("package.json", "{}"),
                ("src/main.ts", "main"),
                ("src/test-utils/build.ts", "helper"),
                ("src/test-utils/__tests__/build.test.ts", "test"),
                ("scripts/release.ts", "script"),
                ("scripts/fixtures/sample.ts", "fixture"),
                ("tools/one.spec.ts", "named file"),
            ],
        );
        let roots = vec!["src".into()];
        let discovered = discover_source_scope(&root, Some(&roots)).unwrap();
        assert_eq!(discovered.source_files, ["src/main.ts"]);

        let roots = vec![
            "src/test-utils".into(),
            "scripts".into(),
            "tools/one.spec.ts".into(),
        ];
        let discovered = discover_source_scope(&root, Some(&roots)).unwrap();
        assert_eq!(
            discovered.source_files,
            [
                "scripts/release.ts",
                "src/test-utils/build.ts",
                "tools/one.spec.ts",
            ]
        );
        assert_eq!(
            entry(&discovered, "src/test-utils/__tests__/build.test.ts").reason,
            "test or fixture source"
        );
        assert_eq!(
            entry(&discovered, "scripts/fixtures/sample.ts").reason,
            "test or fixture source"
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn explicit_roots_are_authoritative_and_outside_files_are_not_limitations() {
        let root = repository(
            "explicit",
            &[
                ("package.json", "{}"),
                ("product/main.ts", "product"),
                ("orphan.ts", "outside"),
            ],
        );
        let roots = vec!["product".into()];
        let discovered = discover_source_scope(&root, Some(&roots)).unwrap();
        assert_eq!(discovered.source_files, ["product/main.ts"]);
        assert_eq!(discovered.scope.mode, SourceScopeMode::Explicit);
        assert_eq!(
            entry(&discovered, "orphan.ts").reason,
            "outside explicit source roots"
        );
        assert!(discovered.limitations.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn parses_jsonc_tsconfig_defaults_and_unicode_paths_without_byte_corruption() {
        let root = repository(
            "jsonc",
            &[
                ("package.json", r#"{"main":"./dist/index.js"}"#),
                (
                    "tsconfig.json",
                    "{ // unicode survives: ž\n \"compilerOptions\": {\"target\": \"es2022\",},\n}",
                ),
                ("events.ts", "event"),
                ("žalias.ts", "unicode"),
                ("library.test.ts", "test"),
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert!(discovered.source_roots.contains(&".".into()));
        assert_eq!(discovered.source_files, ["events.ts", "žalias.ts"]);
        assert!(discovered.limitations.is_empty());
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn never_follows_source_directory_or_explicit_root_symlinks() {
        use std::os::unix::fs::symlink;

        let root = repository(
            "symlink",
            &[("package.json", "{}"), ("src/real.ts", "real")],
        );
        let outside = repository("outside", &[("secret.ts", "secret")]);
        symlink(&outside, root.join("linked")).unwrap();
        symlink(outside.join("secret.ts"), root.join("src/linked.ts")).unwrap();
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(discovered.source_files, ["src/real.ts"]);
        let explicit = vec!["linked".into()];
        assert!(matches!(
            discover_source_scope(&root, Some(&explicit)),
            Err(SourceDiscoveryError::InvalidRoot(_))
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(outside).unwrap();
    }

    #[test]
    fn skips_only_marker_owned_workspace_stores_not_user_supercov_directories() {
        let root = repository(
            "workspace-store",
            &[
                ("package.json", "{}"),
                ("src/main.ts", "main"),
                ("supercov/user.ts", "user code"),
            ],
        );
        let before = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            entry(&before, "supercov/user.ts").status,
            SourceScopeStatus::Ambiguous
        );
        fs::write(
            root.join("supercov/.supercov-workspace-store"),
            b"Supercov instrumented workspace. Safe to delete.\n",
        )
        .unwrap();
        let after = discover_source_scope(&root, None).unwrap();
        assert!(
            after
                .scope
                .entries
                .iter()
                .all(|entry| !entry.file.starts_with("supercov/"))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn declared_workspace_patterns_name_only_real_package_directories() {
        let root = repository(
            "workspace-patterns",
            &[
                (
                    "package.json",
                    r#"{"workspaces":{"packages":["apps/*","./libs/core/","!apps/skip","deep/**","../up","a//b","x*/y","/*","plain-file"]}}"#,
                ),
                (
                    "pnpm-workspace.yaml",
                    "packages:\n  # the tools\n\n  - 'tools/cli'\n  - \"tools/missing\"\ncatalog:\n  - not/a/package\n",
                ),
                ("apps/web/package.json", "{}"),
                // A workspace store Supercov made is no package of the project's.
                ("apps/store/package.json", "{}"),
                (
                    "apps/store/.supercov-workspace-store",
                    "Supercov instrumented workspace. Safe to delete.\n",
                ),
                ("apps/nopkg/index.js", ""),
                ("apps/readme.txt", ""),
                ("libs/core/package.json", "{}"),
                ("tools/cli/package.json", "{}"),
                ("not/a/package/package.json", "{}"),
                ("plain-file", ""),
            ],
        );
        #[cfg(unix)]
        std::os::unix::fs::symlink(root.join("libs/core"), root.join("apps/linked")).unwrap();
        let mut found = declared_workspace_packages(&root)
            .unwrap()
            .into_iter()
            .map(|path| {
                path.strip_prefix(&root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect::<Vec<_>>();
        found.sort();
        assert_eq!(found, ["apps/web", "libs/core", "tools/cli"]);
        // A workspaces list, rather than an object, is read the same way.
        fs::write(root.join("package.json"), r#"{"workspaces":["apps/*"]}"#).unwrap();
        fs::remove_file(root.join("pnpm-workspace.yaml")).unwrap();
        let listed = declared_workspace_packages(&root).unwrap();
        assert_eq!(listed, [root.join("apps/web")]);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn each_source_discovery_error_says_what_failed() {
        use super::SourceDiscoveryError as E;
        let messages = [
            E::Io {
                path: std::path::PathBuf::from("a"),
                source: std::io::Error::other("disk full"),
            },
            E::NonUtf8Path(std::path::PathBuf::from("a")),
            E::InvalidRoot(std::path::PathBuf::from("a")),
            E::OutsideRoot(std::path::PathBuf::from("/a")),
        ]
        .map(|error| error.to_string());
        assert_eq!(
            messages,
            [
                "a: disk full",
                "source path is not valid UTF-8: a",
                "source root is not a regular file or directory: a",
                "source root is outside the project: /a",
            ]
        );
    }
}
