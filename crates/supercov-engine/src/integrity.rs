//! Language-neutral run integrity fingerprints.
//!
//! A frontend contributes only its transformation/runtime shim identity. The
//! Rust engine owns source, test, dependency, configuration and execution
//! fingerprints for every language.

use std::{
    collections::BTreeSet,
    fs,
    io::{self, Read},
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

use crate::{
    project_discovery::CoverageProject,
    run_store::{GitIntegrity, RunFingerprint, RunIntegrity},
};

pub const RUN_INTEGRITY_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontendIntegrityInputs {
    pub language: String,
    pub version: String,
    pub root: PathBuf,
    pub instrumenter_files: Vec<PathBuf>,
    pub execution_files: Vec<PathBuf>,
    pub engine_instrumenter_sha256: String,
    pub engine_execution_sha256: String,
}

impl FrontendIntegrityInputs {
    pub fn javascript(root: PathBuf, runtime_files: Vec<PathBuf>) -> Self {
        Self {
            language: "javascript".into(),
            version: "javascript-v1".into(),
            root,
            instrumenter_files: runtime_files.clone(),
            execution_files: runtime_files,
            engine_instrumenter_sha256: env!("SUPERCOV_JS_FRONTEND_SOURCE_SHA256").into(),
            engine_execution_sha256: env!("SUPERCOV_ENGINE_SOURCE_SHA256").into(),
        }
    }

    pub fn embedded_javascript() -> Self {
        Self {
            language: "javascript".into(),
            version: "javascript-v1".into(),
            root: PathBuf::from("."),
            instrumenter_files: Vec::new(),
            execution_files: Vec::new(),
            engine_instrumenter_sha256: env!("SUPERCOV_JS_FRONTEND_SOURCE_SHA256").into(),
            engine_execution_sha256: env!("SUPERCOV_ENGINE_SOURCE_SHA256").into(),
        }
    }

    pub fn embedded_rust() -> Self {
        Self {
            language: "rust".into(),
            version: "rust-owned-v1".into(),
            root: PathBuf::from("."),
            instrumenter_files: Vec::new(),
            execution_files: Vec::new(),
            engine_instrumenter_sha256: env!("SUPERCOV_ENGINE_SOURCE_SHA256").into(),
            engine_execution_sha256: env!("SUPERCOV_ENGINE_SOURCE_SHA256").into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplicitIntegrityInputs {
    pub source_files: Vec<PathBuf>,
    pub test_files: Vec<PathBuf>,
    pub dependency_files: Vec<PathBuf>,
    pub configuration_files: Vec<PathBuf>,
    pub execution_configuration: Vec<u8>,
}

#[derive(Debug)]
pub enum IntegrityError {
    Io { path: PathBuf, source: io::Error },
    UnsafeFile(PathBuf),
    NonUtf8Path(PathBuf),
    OutsideRoot { root: PathBuf, path: PathBuf },
    InvalidEngineDigest(&'static str),
}

impl std::fmt::Display for IntegrityError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::UnsafeFile(path) => {
                write!(
                    formatter,
                    "fingerprint input is not a regular file: {}",
                    path.display()
                )
            }
            Self::NonUtf8Path(path) => {
                write!(
                    formatter,
                    "fingerprint path is not valid UTF-8: {}",
                    path.display()
                )
            }
            Self::OutsideRoot { root, path } => write!(
                formatter,
                "fingerprint input {} is outside {}",
                path.display(),
                root.display()
            ),
            Self::InvalidEngineDigest(field) => write!(formatter, "invalid {field} SHA-256"),
        }
    }
}

impl std::error::Error for IntegrityError {}

fn io_error(path: &Path, source: io::Error) -> IntegrityError {
    IntegrityError::Io {
        path: path.to_owned(),
        source,
    }
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn local_path(root: &Path, path: &Path) -> Result<String, IntegrityError> {
    let path = path
        .strip_prefix(root)
        .map_err(|_| IntegrityError::OutsideRoot {
            root: root.to_owned(),
            path: path.to_owned(),
        })?;
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(str::to_owned)
                .ok_or_else(|| IntegrityError::NonUtf8Path(path.to_owned()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

fn digest_files(
    root: &Path,
    paths: impl IntoIterator<Item = PathBuf>,
) -> Result<String, IntegrityError> {
    let paths = paths.into_iter().collect::<BTreeSet<_>>();
    let mut labeled = paths
        .into_iter()
        .map(|path| local_path(root, &path).map(|label| (label, path)))
        .collect::<Result<Vec<_>, _>>()?;
    labeled.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hash = Sha256::new();
    let mut buffer = [0_u8; 128 * 1024];
    for (label, path) in labeled {
        let metadata = fs::symlink_metadata(&path).map_err(|source| io_error(&path, source))?;
        if !metadata.file_type().is_file() {
            return Err(IntegrityError::UnsafeFile(path));
        }
        hash.update(label.as_bytes());
        hash.update([0]);
        let mut file = fs::File::open(&path).map_err(|source| io_error(&path, source))?;
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|source| io_error(&path, source))?;
            if read == 0 {
                break;
            }
            hash.update(&buffer[..read]);
        }
        hash.update([0]);
    }
    Ok(format!("{:x}", hash.finalize()))
}

fn domain_hash(domain: &str, fields: &[(&str, &[u8])]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain.as_bytes());
    hash.update([0]);
    for (name, value) in fields {
        hash.update((*name).len().to_le_bytes());
        hash.update(name.as_bytes());
        hash.update(value.len().to_le_bytes());
        hash.update(value);
    }
    format!("{:x}", hash.finalize())
}

fn source_file(path: &Path) -> bool {
    let lower = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    [
        ".js", ".jsx", ".ts", ".tsx", ".cjs", ".cjsx", ".cts", ".ctsx", ".mjs", ".mjsx", ".mts",
        ".mtsx",
    ]
    .iter()
    .any(|extension| lower.ends_with(extension))
}

fn skipped_directory(name: &str) -> bool {
    [
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
    ]
    .contains(&name)
}

fn owned_workspace_store(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "supercov")
        && path.join(".supercov-workspace-store").is_file()
}

fn walk_files(
    directory: &Path,
    predicate: &impl Fn(&Path) -> bool,
    output: &mut Vec<PathBuf>,
) -> Result<(), IntegrityError> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_error(directory, source)),
    };
    if !metadata.file_type().is_dir() {
        return Err(IntegrityError::UnsafeFile(directory.to_owned()));
    }
    let mut entries = fs::read_dir(directory)
        .map_err(|source| io_error(directory, source))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| io_error(directory, source))?;
    entries.sort_by_key(fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|source| io_error(&path, source))?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            let name = entry.file_name();
            if !name.to_str().is_some_and(skipped_directory) && !owned_workspace_store(&path) {
                walk_files(&path, predicate, output)?;
            }
        } else if file_type.is_file() && predicate(&path) {
            output.push(path);
        }
    }
    Ok(())
}

fn test_file(root: &Path, path: &Path) -> bool {
    if !source_file(path) {
        return false;
    }
    let local = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    local
        .to_ascii_lowercase()
        .split(['/', '\\', '_', '.', '-'])
        .any(|part| matches!(part, "test" | "spec"))
}

fn test_files(root: &Path) -> Result<Vec<PathBuf>, IntegrityError> {
    let mut files = Vec::new();
    for directory in ["test", "tests", "__tests__"] {
        walk_files(&root.join(directory), &source_file, &mut files)?;
    }
    walk_files(root, &|path| test_file(root, path), &mut files)?;
    files.sort();
    files.dedup();
    Ok(files)
}

fn dependency_files(root: &Path) -> Result<Vec<PathBuf>, IntegrityError> {
    let mut files = Vec::new();
    walk_files(
        root,
        &|path| path.file_name().is_some_and(|name| name == "package.json"),
        &mut files,
    )?;
    for name in [
        "package-lock.json",
        "npm-shrinkwrap.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
    ] {
        let path = root.join(name);
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    Ok(files)
}

fn configuration_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    name == ".npmrc"
        || name == "supercov.waivers.json"
        || (name.starts_with("tsconfig") && name.ends_with(".json"))
        || name.contains(".config.")
        || name.starts_with(".babelrc.")
        || name.starts_with(".eslint")
        || name.starts_with(".prettier")
}

fn configuration_files(
    root: &Path,
    project: &CoverageProject,
) -> Result<Vec<PathBuf>, IntegrityError> {
    let mut files = Vec::new();
    walk_files(root, &configuration_file, &mut files)?;
    files.extend(
        [
            project.playwright_config.as_ref(),
            project.vitest_config.as_ref(),
            project.jest_config.as_ref(),
        ]
        .into_iter()
        .flatten()
        .cloned(),
    );
    files.sort();
    files.dedup();
    Ok(files)
}

fn git_integrity(root: &Path) -> Option<GitIntegrity> {
    let revision = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(root)
        .output()
        .ok();
    let status = Command::new("git")
        .args(["status", "--porcelain=v1"])
        .current_dir(root)
        .output()
        .ok();
    if !revision
        .as_ref()
        .is_some_and(|output| output.status.success())
        && !status
            .as_ref()
            .is_some_and(|output| output.status.success())
    {
        return None;
    }
    Some(GitIntegrity {
        revision: revision
            .filter(|output| output.status.success())
            .and_then(|output| String::from_utf8(output.stdout).ok())
            .map(|revision| revision.trim().to_owned()),
        dirty: !status
            .as_ref()
            .is_some_and(|output| output.status.success() && output.stdout.is_empty()),
    })
}

pub fn create_run_integrity(
    root: &Path,
    project: &CoverageProject,
    frontend: &FrontendIntegrityInputs,
) -> Result<RunIntegrity, IntegrityError> {
    if !valid_sha256(&frontend.engine_instrumenter_sha256) {
        return Err(IntegrityError::InvalidEngineDigest("instrumenter engine"));
    }
    if !valid_sha256(&frontend.engine_execution_sha256) {
        return Err(IntegrityError::InvalidEngineDigest("execution engine"));
    }
    let source_paths = project
        .source_files
        .iter()
        .map(|path| root.join(path))
        .collect::<Vec<_>>();
    let tests = test_files(root)?;
    let dependencies = dependency_files(root)?;
    let configuration = configuration_files(root, project)?;
    let source = digest_files(root, source_paths)?;
    let tests_digest = digest_files(root, tests.iter().cloned())?;
    let dependency_digest = digest_files(root, dependencies)?;
    let configuration_digest = digest_files(root, configuration)?;
    let frontend_instrumenter =
        digest_files(&frontend.root, frontend.instrumenter_files.iter().cloned())?;
    let frontend_execution =
        digest_files(&frontend.root, frontend.execution_files.iter().cloned())?;
    let instrumenter = domain_hash(
        "supercov-run-instrumenter-v1",
        &[
            ("language", frontend.language.as_bytes()),
            ("version", frontend.version.as_bytes()),
            ("engine", frontend.engine_instrumenter_sha256.as_bytes()),
            ("shim", frontend_instrumenter.as_bytes()),
        ],
    );
    let build_environment = frontend_map_bytes(&project.build_environment);
    let execution = domain_hash(
        "supercov-run-execution-v1",
        &[
            ("language", frontend.language.as_bytes()),
            ("version", frontend.version.as_bytes()),
            ("source", source.as_bytes()),
            ("dependencies", dependency_digest.as_bytes()),
            ("configuration", configuration_digest.as_bytes()),
            ("buildEnvironment", &build_environment),
            ("engine", frontend.engine_execution_sha256.as_bytes()),
            ("shim", frontend_execution.as_bytes()),
        ],
    );
    let combined = domain_hash(
        "supercov-run-combined-v1",
        &[
            ("language", frontend.language.as_bytes()),
            ("version", frontend.version.as_bytes()),
            ("source", source.as_bytes()),
            ("tests", tests_digest.as_bytes()),
            ("dependencies", dependency_digest.as_bytes()),
            ("configuration", configuration_digest.as_bytes()),
            ("instrumenter", instrumenter.as_bytes()),
        ],
    );
    Ok(RunIntegrity {
        schema_version: RUN_INTEGRITY_SCHEMA_VERSION,
        instrumenter_version: frontend.version.clone(),
        git: git_integrity(root),
        fingerprint: RunFingerprint {
            algorithm: "sha256".into(),
            source,
            tests: tests_digest,
            dependencies: dependency_digest,
            configuration: configuration_digest,
            instrumenter,
            execution,
            combined,
            source_files: project.source_files.len(),
            test_files: tests.len(),
        },
        stale: None,
        stale_reasons: None,
    })
}

/// Language-neutral integrity construction for frontends whose discovery does
/// not use the JavaScript `CoverageProject` compatibility structure.
pub fn create_explicit_run_integrity(
    root: &Path,
    inputs: &ExplicitIntegrityInputs,
    frontend: &FrontendIntegrityInputs,
) -> Result<RunIntegrity, IntegrityError> {
    if !valid_sha256(&frontend.engine_instrumenter_sha256) {
        return Err(IntegrityError::InvalidEngineDigest("instrumenter engine"));
    }
    if !valid_sha256(&frontend.engine_execution_sha256) {
        return Err(IntegrityError::InvalidEngineDigest("execution engine"));
    }
    let source = digest_files(root, inputs.source_files.iter().map(|path| root.join(path)))?;
    let tests = digest_files(root, inputs.test_files.iter().map(|path| root.join(path)))?;
    let dependencies = digest_files(
        root,
        inputs.dependency_files.iter().map(|path| root.join(path)),
    )?;
    let configuration = digest_files(
        root,
        inputs
            .configuration_files
            .iter()
            .map(|path| root.join(path)),
    )?;
    let frontend_instrumenter =
        digest_files(&frontend.root, frontend.instrumenter_files.iter().cloned())?;
    let frontend_execution =
        digest_files(&frontend.root, frontend.execution_files.iter().cloned())?;
    let instrumenter = domain_hash(
        "supercov-run-instrumenter-v1",
        &[
            ("language", frontend.language.as_bytes()),
            ("version", frontend.version.as_bytes()),
            ("engine", frontend.engine_instrumenter_sha256.as_bytes()),
            ("shim", frontend_instrumenter.as_bytes()),
        ],
    );
    let execution = domain_hash(
        "supercov-run-execution-v1",
        &[
            ("language", frontend.language.as_bytes()),
            ("version", frontend.version.as_bytes()),
            ("source", source.as_bytes()),
            ("dependencies", dependencies.as_bytes()),
            ("configuration", configuration.as_bytes()),
            ("executionConfiguration", &inputs.execution_configuration),
            ("engine", frontend.engine_execution_sha256.as_bytes()),
            ("shim", frontend_execution.as_bytes()),
        ],
    );
    let combined = domain_hash(
        "supercov-run-combined-v1",
        &[
            ("language", frontend.language.as_bytes()),
            ("version", frontend.version.as_bytes()),
            ("source", source.as_bytes()),
            ("tests", tests.as_bytes()),
            ("dependencies", dependencies.as_bytes()),
            ("configuration", configuration.as_bytes()),
            ("instrumenter", instrumenter.as_bytes()),
            ("execution", execution.as_bytes()),
        ],
    );
    Ok(RunIntegrity {
        schema_version: RUN_INTEGRITY_SCHEMA_VERSION,
        instrumenter_version: format!("supercov-{}-{}", frontend.language, frontend.version),
        git: git_integrity(root),
        fingerprint: RunFingerprint {
            // The frozen store contract names the digest primitive here. The
            // domain-separation version belongs to the producer implementation,
            // not this wire field.
            algorithm: "sha256".into(),
            source,
            tests,
            dependencies,
            configuration,
            instrumenter,
            execution,
            combined,
            source_files: inputs.source_files.len(),
            test_files: inputs.test_files.len(),
        },
        stale: None,
        stale_reasons: None,
    })
}

fn frontend_map_bytes(values: &std::collections::BTreeMap<String, String>) -> Vec<u8> {
    let mut bytes = Vec::new();
    for (key, value) in values {
        bytes.extend_from_slice(&key.len().to_le_bytes());
        bytes.extend_from_slice(key.as_bytes());
        bytes.extend_from_slice(&value.len().to_le_bytes());
        bytes.extend_from_slice(value.as_bytes());
    }
    bytes
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeMap,
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use crate::{project_discovery::discover_coverage_project, run_store::compare_run_integrity};

    use super::*;

    static TEMPORARY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

    fn directory(label: &str) -> PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-integrity-{label}-{}-{nonce}-{}",
            std::process::id(),
            TEMPORARY_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir_all(&root).unwrap();
        root
    }

    fn write(root: &Path, path: &str, contents: &str) {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn frontend(root: &Path) -> FrontendIntegrityInputs {
        FrontendIntegrityInputs {
            language: "javascript".into(),
            version: "javascript-v1".into(),
            root: root.to_owned(),
            instrumenter_files: vec![root.join("instrumenter.js")],
            execution_files: vec![root.join("runtime.js")],
            engine_instrumenter_sha256: env!("SUPERCOV_JS_FRONTEND_SOURCE_SHA256").into(),
            engine_execution_sha256: env!("SUPERCOV_ENGINE_SOURCE_SHA256").into(),
        }
    }

    fn fixture() -> (PathBuf, PathBuf) {
        let root = directory("project");
        let shim = directory("shim");
        write(
            &root,
            "package.json",
            r#"{"scripts":{"build":"vite build","test":"node --test"}}"#,
        );
        write(&root, "package-lock.json", "lock");
        write(&root, "src/index.ts", "export const ready = true");
        write(&root, "tests/index.test.ts", "test('ready', () => {})");
        write(&root, "vite.config.ts", "export default {}");
        write(&root, ".cache/test262/fake.test.js", "ignored");
        write(&root, "supercov/.supercov-workspace-store", "owned");
        write(
            &root,
            "supercov/workspace/copy/tests/copied.test.ts",
            "ignored copied test",
        );
        write(&shim, "instrumenter.js", "instrument");
        write(&shim, "runtime.js", "runtime");
        (root, shim)
    }

    fn integrity(root: &Path, shim: &Path, environment: &BTreeMap<String, String>) -> RunIntegrity {
        let project = discover_coverage_project(root, environment, &[]).unwrap();
        create_run_integrity(root, &project, &frontend(shim)).unwrap()
    }

    #[test]
    fn fingerprints_every_independent_input_domain_deterministically() {
        let (root, shim) = fixture();
        let first = integrity(&root, &shim, &BTreeMap::new());
        let second = integrity(&root, &shim, &BTreeMap::new());
        assert_eq!(first, second);
        assert_eq!(first.fingerprint.source_files, 1);
        assert_eq!(first.fingerprint.test_files, 1);
        for digest in [
            &first.fingerprint.source,
            &first.fingerprint.tests,
            &first.fingerprint.dependencies,
            &first.fingerprint.configuration,
            &first.fingerprint.instrumenter,
            &first.fingerprint.execution,
            &first.fingerprint.combined,
        ] {
            assert!(valid_sha256(digest));
        }

        write(&root, "src/index.ts", "export const ready = false");
        let source = integrity(&root, &shim, &BTreeMap::new());
        assert_ne!(source.fingerprint.source, first.fingerprint.source);
        assert_eq!(source.fingerprint.tests, first.fingerprint.tests);
        assert_ne!(source.fingerprint.execution, first.fingerprint.execution);

        write(&root, "src/index.ts", "export const ready = true");
        write(&root, "tests/index.test.ts", "test('changed', () => {})");
        let tests = integrity(&root, &shim, &BTreeMap::new());
        assert_eq!(tests.fingerprint.source, first.fingerprint.source);
        assert_ne!(tests.fingerprint.tests, first.fingerprint.tests);
        assert_eq!(tests.fingerprint.execution, first.fingerprint.execution);

        write(&root, "tests/index.test.ts", "test('ready', () => {})");
        write(&root, "package-lock.json", "changed lock");
        let dependencies = integrity(&root, &shim, &BTreeMap::new());
        assert_ne!(
            dependencies.fingerprint.dependencies,
            first.fingerprint.dependencies
        );
        assert_ne!(
            dependencies.fingerprint.execution,
            first.fingerprint.execution
        );

        write(&root, "package-lock.json", "lock");
        write(&root, "vite.config.ts", "export default { changed: true }");
        let configuration = integrity(&root, &shim, &BTreeMap::new());
        assert_ne!(
            configuration.fingerprint.configuration,
            first.fingerprint.configuration
        );

        write(&root, "vite.config.ts", "export default {}");
        write(&shim, "instrumenter.js", "changed instrumenter");
        let instrumenter = integrity(&root, &shim, &BTreeMap::new());
        assert_ne!(
            instrumenter.fingerprint.instrumenter,
            first.fingerprint.instrumenter
        );
        assert_ne!(
            instrumenter.fingerprint.combined,
            first.fingerprint.combined
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(shim).unwrap();
    }

    #[test]
    fn fingerprints_nested_workspace_manifests_and_execution_environment() {
        let (root, shim) = fixture();
        write(
            &root,
            "packages/ui/package.json",
            r#"{"dependencies":{"react":"1"}}"#,
        );
        write(&root, "packages/ui/src/index.ts", "export const ui = true");
        let first = integrity(&root, &shim, &BTreeMap::new());
        write(
            &root,
            "packages/ui/package.json",
            r#"{"dependencies":{"react":"2"}}"#,
        );
        let dependency = integrity(&root, &shim, &BTreeMap::new());
        assert_ne!(
            first.fingerprint.dependencies,
            dependency.fingerprint.dependencies
        );

        let mut environment = BTreeMap::new();
        environment.insert("SUPERCOV_SOURCE_ROOTS".into(), "src,packages/ui/src".into());
        let project = discover_coverage_project(&root, &environment, &[]).unwrap();
        let mut project_with_build_environment = project.clone();
        project_with_build_environment
            .build_environment
            .insert("MODE".into(), "test".into());
        let changed =
            create_run_integrity(&root, &project_with_build_environment, &frontend(&shim)).unwrap();
        let baseline = create_run_integrity(&root, &project, &frontend(&shim)).unwrap();
        assert_ne!(
            baseline.fingerprint.execution,
            changed.fingerprint.execution
        );
        assert_eq!(baseline.fingerprint.combined, changed.fingerprint.combined);
        assert_eq!(
            compare_run_integrity(Some(&baseline), &changed).reasons,
            ["execution environment changed"]
        );
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(shim).unwrap();
    }

    #[test]
    fn explicit_language_integrity_uses_the_frozen_store_digest_label() {
        let root = directory("rust-project");
        write(&root, "src/lib.rs", "pub fn ready() -> bool { true }");
        write(
            &root,
            "Cargo.toml",
            "[package]\nname='fixture'\nversion='0.0.0'\n",
        );
        let inputs = ExplicitIntegrityInputs {
            source_files: vec!["src/lib.rs".into()],
            test_files: vec!["src/lib.rs".into()],
            dependency_files: vec!["Cargo.toml".into()],
            configuration_files: Vec::new(),
            execution_configuration: b"cargo\0test".to_vec(),
        };
        let result = create_explicit_run_integrity(
            &root,
            &inputs,
            &FrontendIntegrityInputs::embedded_rust(),
        )
        .unwrap();
        assert_eq!(result.fingerprint.algorithm, "sha256");
        fs::remove_dir_all(root).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_linked_frontend_identity_files() {
        use std::os::unix::fs::symlink;

        let (root, shim) = fixture();
        let outside = shim.join("outside.js");
        fs::write(&outside, "outside").unwrap();
        fs::remove_file(shim.join("instrumenter.js")).unwrap();
        symlink(&outside, shim.join("instrumenter.js")).unwrap();
        let project = discover_coverage_project(&root, &BTreeMap::new(), &[]).unwrap();
        assert!(matches!(
            create_run_integrity(&root, &project, &frontend(&shim)),
            Err(IntegrityError::UnsafeFile(_))
        ));
        fs::remove_dir_all(root).unwrap();
        fs::remove_dir_all(shim).unwrap();
    }
}
