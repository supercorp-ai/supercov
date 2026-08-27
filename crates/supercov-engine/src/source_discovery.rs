//! Deterministic first-party JavaScript/TypeScript source discovery.
//!
//! Discovery defines the coverage denominator. The walker never follows
//! links and turns unclassified first-party files into explicit blockers.

use std::{
    collections::BTreeSet,
    fs, io,
    path::{Component, Path, PathBuf},
};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

const GENERATED_DIRECTORIES: &[&str] = &[
    ".cache",
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
const SOURCE_DIRECTORIES: &[&str] = &["app", "src", "lib", "server", "client", "functions", "api"];
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SourceScopeEntry {
    pub file: String,
    pub status: SourceScopeStatus,
    pub reason: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub package_root: Option<String>,
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

fn lexical_normalize(path: &Path) -> PathBuf {
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
        .map_err(|_| SourceDiscoveryError::InvalidRoot(path.to_owned()))?;
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

fn generated_directory(name: &str) -> bool {
    GENERATED_DIRECTORIES.contains(&name)
}

fn owned_workspace_store(path: &Path) -> bool {
    crate::workspace::owned_workspace_path(path)
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

fn files_under(directory: &Path, output: &mut Vec<PathBuf>) -> Result<(), SourceDiscoveryError> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| io_error(directory, error))?;
    if !metadata.file_type().is_dir() {
        return Err(SourceDiscoveryError::InvalidRoot(directory.to_owned()));
    }
    for entry in read_directory(directory)? {
        let name = entry
            .file_name()
            .into_string()
            .map_err(|name| SourceDiscoveryError::NonUtf8Path(directory.join(name)))?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| io_error(&path, error))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            if !generated_directory(&name) && !owned_workspace_store(&path) {
                files_under(&path, output)?;
            }
        } else if file_type.is_file() && source_file(&name) {
            output.push(path);
        }
    }
    Ok(())
}

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
            let file_type = entry.file_type().map_err(|error| io_error(&path, error))?;
            if !file_type.is_dir()
                || file_type.is_symlink()
                || generated_directory(&name)
                || owned_workspace_store(&path)
            {
                continue;
            }
            let local = local_path(root, &path)?;
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
    Ok(found.into_iter().collect())
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
            let prefix = target.split('*').next()?.trim_end_matches('/');
            (!prefix.is_empty()).then(|| resolve(directory, prefix))
        })
        .collect()
}

fn strip_jsonc_comments(contents: &str) -> String {
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

fn strip_trailing_commas(contents: &str) -> String {
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
            (!prefix.is_empty()).then(|| resolve(directory, prefix))
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
        reason: "First-party JavaScript/TypeScript source could not be classified automatically. Configure SUPERCOV_SOURCE_ROOTS or move it under a discovered package source root.".into(),
    }
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
                SOURCE_DIRECTORIES
                    .iter()
                    .map(|name| directory.join(name))
                    .chain(entry_targets(directory, &manifest))
                    .chain(tsconfig_roots(directory))
                    .collect::<Vec<_>>()
            })
            .collect()
    };
    let mut existing_roots = BTreeSet::new();
    for path in include_roots {
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_file() || metadata.file_type().is_dir() => {
                existing_roots.insert(path);
            }
            Ok(_) => return Err(SourceDiscoveryError::InvalidRoot(path)),
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error(&path, error)),
        }
    }
    let existing_roots = existing_roots.into_iter().collect::<Vec<_>>();
    let mut all_files = Vec::new();
    files_under(&root, &mut all_files)?;
    all_files.sort();

    let mut entries = Vec::new();
    let mut included = Vec::new();
    let mut limitations = Vec::new();
    for path in all_files {
        let file = local_path(&root, &path)?;
        let package_root = nearest_package_root(&path, &packages)
            .map(|package| local_path(&root, package))
            .transpose()?
            .filter(|package| package != ".");
        let entry = |status, reason: &str| SourceScopeEntry {
            file: file.clone(),
            status,
            reason: reason.into(),
            package_root: package_root.clone(),
        };
        if declaration_file(&file) {
            entries.push(entry(SourceScopeStatus::Excluded, "TypeScript declaration"));
        } else if test_or_fixture(&file) {
            entries.push(entry(SourceScopeStatus::Excluded, "test or fixture source"));
        } else if tool_script(&file) {
            entries.push(entry(
                SourceScopeStatus::Excluded,
                "conventional tool script",
            ));
        } else if config_file(&file) {
            entries.push(entry(
                SourceScopeStatus::Excluded,
                "build/test/tool configuration",
            ));
        } else if existing_roots.iter().any(|directory| {
            fs::symlink_metadata(directory)
                .map(|metadata| {
                    if metadata.file_type().is_dir() {
                        within(directory, &path)
                    } else {
                        directory == &path
                    }
                })
                .unwrap_or(false)
        }) {
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
    let source_files = included
        .iter()
        .map(|path| local_path(&root, path))
        .collect::<Result<Vec<_>, _>>()?;
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

    fn entry<'a>(scope: &'a DiscoveredSourceScope, file: &str) -> &'a SourceScopeEntry {
        scope
            .scope
            .entries
            .iter()
            .find(|entry| entry.file == file)
            .unwrap()
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
            ],
        );
        let discovered = discover_source_scope(&root, None).unwrap();
        assert_eq!(
            discovered.source_files,
            ["lib/helper.js", "packages/ui/src/index.ts", "src/index.ts"]
        );
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
}
