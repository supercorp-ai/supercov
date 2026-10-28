//! Exact-input stable workspace metadata for the owned Rust frontend.

use crate::fault_fs as fs;
use crate::fault_process::Launch;
use std::{
    path::{Path, PathBuf},
    process::Command,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{coverage_report::CoverageManifest, lifecycle::atomic_write, run_store::RunIntegrity};

pub const RUST_BUILD_CACHE_SCHEMA_VERSION: u32 = 5;
const CACHE_FILE: &str = ".supercov/rust-build-cache.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustArtifactFingerprint {
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RustBuildCacheMetadata {
    pub schema_version: u32,
    pub key: String,
    pub created_at: String,
    pub source_files: Vec<String>,
    pub instrumented_source_sha256: String,
    pub artifacts: Vec<RustArtifactFingerprint>,
    pub manifest: CoverageManifest,
    /// The probes' numbering the cached build was compiled with.
    pub probes: crate::rust_instrumenter::RustProbeLayout,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RustBuildCacheIdentity<'a> {
    schema_version: u32,
    source_fingerprint: &'a str,
    dependencies_fingerprint: &'a str,
    configuration_fingerprint: &'a str,
    /// The run's execution configuration with what follows the command's
    /// `--` left out: libtest's arguments choose the tests a build runs, and
    /// a run of other tests reuses the same build.
    build_configuration: String,
    /// Supercov's own build: cached output from a different instrumenter
    /// must never be reused.
    instrumenter_fingerprint: &'a str,
    rustc: String,
    cargo: String,
    platform: &'static str,
    architecture: &'static str,
}

fn tool_version(program: &str, arguments: &[&str]) -> String {
    Command::new(program)
        .args(arguments)
        .launch_output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .unwrap_or("unavailable".into())
}

/// `execution_configuration` is the run's: its command joined by NULs, then
/// what else the run's integrity folds in after it.
pub fn rust_build_cache_key(
    integrity: &RunIntegrity,
    execution_configuration: &[u8],
    command: &[String],
) -> String {
    let joined = command.join("\0");
    let rest = execution_configuration
        .strip_prefix(joined.as_bytes())
        .expect("the execution configuration starts with the command");
    let build_command = command
        .split(|argument| argument == "--")
        .next()
        .expect("a split yields a part at least");
    let mut build_configuration = build_command.join("\0").into_bytes();
    build_configuration.extend_from_slice(rest);
    let identity = RustBuildCacheIdentity {
        schema_version: RUST_BUILD_CACHE_SCHEMA_VERSION,
        source_fingerprint: &integrity.fingerprint.source,
        dependencies_fingerprint: &integrity.fingerprint.dependencies,
        configuration_fingerprint: &integrity.fingerprint.configuration,
        build_configuration: format!("{:x}", Sha256::digest(&build_configuration)),
        instrumenter_fingerprint: &integrity.fingerprint.instrumenter,
        rustc: tool_version("rustc", &["-vV"]),
        cargo: tool_version("cargo", &["-Vv"]),
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
    };
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&identity).expect("plain data serializes"))
    )
}

fn regular_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn safe_relative(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, std::path::Component::Normal(_)))
}

fn digest_files(root: &Path, files: &[String]) -> Option<String> {
    let mut digest = Sha256::new();
    for relative in files {
        if !safe_relative(relative) {
            return None;
        }
        let path = root.join(relative);
        let metadata = fs::symlink_metadata(&path).ok()?;
        if !metadata.file_type().is_file() {
            return None;
        }
        let bytes = fs::read(path).ok()?;
        digest.update((relative.len() as u64).to_le_bytes());
        digest.update(relative.as_bytes());
        digest.update((bytes.len() as u64).to_le_bytes());
        digest.update(bytes);
    }
    Some(format!("{:x}", digest.finalize()))
}

fn fingerprint_artifacts(
    target_directory: &Path,
    artifacts: &[PathBuf],
) -> Result<Vec<RustArtifactFingerprint>, String> {
    let canonical_target = fs::canonicalize(target_directory).map_err(|error| error.to_string())?;
    let mut fingerprints = Vec::new();
    for artifact in artifacts {
        let artifact = fs::canonicalize(artifact).map_err(|error| error.to_string())?;
        let relative = artifact
            .strip_prefix(&canonical_target)
            .map_err(|_| {
                format!(
                    "cached Rust artifact escaped target: {}",
                    artifact.display()
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        if !safe_relative(&relative) {
            return Err(format!("unsafe cached Rust artifact: {relative}"));
        }
        let metadata = fs::symlink_metadata(&artifact).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_file() {
            return Err(format!(
                "cached Rust artifact is not a regular file: {}",
                artifact.display()
            ));
        }
        let bytes = fs::read(&artifact).map_err(|error| error.to_string())?;
        fingerprints.push(RustArtifactFingerprint {
            path: relative,
            bytes: bytes.len() as u64,
            sha256: format!("{:x}", Sha256::digest(bytes)),
        });
    }
    fingerprints.sort_by(|left, right| left.path.cmp(&right.path));
    fingerprints.dedup_by(|left, right| left.path == right.path);
    Ok(fingerprints)
}

fn artifacts_match(target_directory: &Path, expected: &[RustArtifactFingerprint]) -> bool {
    if expected.is_empty() || expected.windows(2).any(|pair| pair[0].path >= pair[1].path) {
        return false;
    }
    expected.iter().all(|expected| {
        if !safe_relative(&expected.path) {
            return false;
        }
        let path = target_directory.join(&expected.path);
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            return false;
        };
        if !metadata.file_type().is_file() || metadata.len() != expected.bytes {
            return false;
        }
        fs::read(path).is_ok_and(|bytes| format!("{:x}", Sha256::digest(bytes)) == expected.sha256)
    })
}

pub fn read_rust_build_cache(
    workspace: &Path,
    target_directory: &Path,
    key: &str,
) -> Option<RustBuildCacheMetadata> {
    if !regular_directory(workspace) || !regular_directory(target_directory) {
        return None;
    }
    let path = workspace.join(CACHE_FILE);
    if !fs::symlink_metadata(&path).is_ok_and(|metadata| metadata.file_type().is_file()) {
        return None;
    }
    let metadata: RustBuildCacheMetadata = serde_json::from_slice(&fs::read(path).ok()?).ok()?;
    if metadata.schema_version != RUST_BUILD_CACHE_SCHEMA_VERSION
        || metadata.key != key
        || metadata.source_files.is_empty()
        || metadata.manifest.points.is_empty()
        || metadata
            .source_files
            .windows(2)
            .any(|pair| pair[0] >= pair[1])
        || digest_files(workspace, &metadata.source_files).as_deref()
            != Some(metadata.instrumented_source_sha256.as_str())
        || !artifacts_match(target_directory, &metadata.artifacts)
    {
        return None;
    }
    Some(metadata)
}

// The manifest and the probes' numbering are the instrumentation the cached
// build was compiled with; they are written together.
#[allow(clippy::too_many_arguments)]
pub fn write_rust_build_cache(
    project_root: &Path,
    workspace: &Path,
    key: &str,
    created_at: &str,
    source_files: &[String],
    manifest: &CoverageManifest,
    probes: &crate::rust_instrumenter::RustProbeLayout,
    artifacts: &[PathBuf],
) -> Result<RustBuildCacheMetadata, String> {
    let mut source_files = source_files.to_vec();
    source_files.sort();
    source_files.dedup();
    let instrumented_source_sha256 = digest_files(workspace, &source_files)
        .ok_or_else(|| "could not authenticate instrumented Rust sources".to_owned())?;
    let target_directory = rust_target_directory(project_root);
    let metadata = RustBuildCacheMetadata {
        schema_version: RUST_BUILD_CACHE_SCHEMA_VERSION,
        key: key.into(),
        created_at: created_at.into(),
        source_files,
        instrumented_source_sha256,
        artifacts: fingerprint_artifacts(&target_directory, artifacts)?,
        manifest: manifest.clone(),
        probes: probes.clone(),
    };
    let mut bytes = serde_json::to_vec_pretty(&metadata).expect("plain data serializes");
    bytes.push(b'\n');
    atomic_write(project_root, &workspace.join(CACHE_FILE), &bytes)
        .map_err(|error| error.to_string())?;
    Ok(metadata)
}

pub fn rust_target_directory(project_root: &Path) -> PathBuf {
    project_root.join(".supercov/cache/rust-target")
}

#[cfg(test)]
mod tests {
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::{coverage_analysis::PointKind, coverage_report::PointMeta};

    /// A run of other tests reuses the build: what follows the command's
    /// `--` is left out of the key, and nothing else is.
    #[test]
    fn a_build_is_keyed_on_all_but_the_tests_a_command_chooses() {
        let mut integrity: RunIntegrity = serde_json::from_value(serde_json::json!({
            "schemaVersion": 2,
            "instrumenterVersion": "supercov-rust-1",
            "fingerprint": {
                "algorithm": "sha256", "source": "a", "tests": "b", "dependencies": "c",
                "configuration": "d", "instrumenter": "e", "execution": "f", "combined": "0",
                "sourceFiles": 1, "testFiles": 1
            }
        }))
        .unwrap();
        let command = |words: &str| words.split(' ').map(str::to_owned).collect::<Vec<_>>();
        let key = |integrity: &RunIntegrity, words: &str, links: &str| {
            let command = command(words);
            let mut configuration = command.join("\0").into_bytes();
            configuration.extend_from_slice(links.as_bytes());
            rust_build_cache_key(integrity, &configuration, &command)
        };
        let base = key(&integrity, "cargo test -- --exact a", "\0link");
        assert_eq!(base, key(&integrity, "cargo test -- --exact b c", "\0link"));
        assert_eq!(key(&integrity, "cargo test", "\0link"), base);
        assert_ne!(
            key(&integrity, "cargo test --release -- --exact a", "\0link"),
            base
        );
        assert_ne!(key(&integrity, "cargo test -- --exact a", "\0other"), base);
        integrity.fingerprint.source = "changed".into();
        assert_ne!(key(&integrity, "cargo test -- --exact a", "\0link"), base);
    }

    /// A cached build's files are named relative to their root, digested in
    /// order, and its artifacts match only by path, order, size and digest;
    /// one outside the target or not a file is refused when fingerprinted.
    #[test]
    fn cached_files_and_artifacts_match_exactly() {
        for (value, safe) in [
            ("src/lib.rs", true),
            ("", false),
            ("/abs/lib.rs", false),
            ("../lib.rs", false),
            ("./lib.rs", false),
        ] {
            assert_eq!(safe_relative(value), safe, "{value:?}");
        }
        let root = std::env::temp_dir().join(format!(
            "supercov-build-cache-files-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(root.join("target/debug")).unwrap();
        let root = crate::test_directory::TestDirectory::adopt(root);
        fs::write(root.join("a.rs"), "a").unwrap();
        fs::create_dir(root.join("dir")).unwrap();
        assert!(digest_files(&root, &["a.rs".into()]).is_some());
        assert!(digest_files(&root, &[]).is_some());
        assert_eq!(digest_files(&root, &["../a.rs".into()]), None);
        assert_eq!(digest_files(&root, &["dir".into()]), None);
        assert_eq!(digest_files(&root, &["missing.rs".into()]), None);
        let target = root.join("target");
        fs::write(target.join("debug/bin"), "binary").unwrap();
        fs::write(target.join("debug/other"), "other").unwrap();
        fs::write(root.join("outside"), "x").unwrap();
        let fingerprints = fingerprint_artifacts(
            &target,
            &[
                target.join("debug/other"),
                target.join("debug/bin"),
                target.join("debug/bin"),
            ],
        )
        .unwrap();
        assert_eq!(
            fingerprints
                .iter()
                .map(|f| f.path.as_str())
                .collect::<Vec<_>>(),
            ["debug/bin", "debug/other"]
        );
        assert!(artifacts_match(&target, &fingerprints));
        // Each step of fingerprinting that fails is refused: the target and
        // the artifact resolved, the artifact looked at and read; a file
        // that cannot be read is no digest.
        for fault in [
            "fs.canonicalize",
            "fs.canonicalize@2",
            "fs.symlink_metadata",
            "fs.read",
        ] {
            assert!(
                crate::fault::during(fault, || {
                    fingerprint_artifacts(&target, &[target.join("debug/bin")])
                })
                .is_err(),
                "{fault}"
            );
        }
        assert_eq!(
            crate::fault::during("fs.read", || digest_files(&root, &["a.rs".into()])),
            None
        );
        assert!(fingerprint_artifacts(&target, &[root.join("outside")]).is_err());
        assert!(fingerprint_artifacts(&target, &[target.join("debug")]).is_err());
        // The target directory itself names no artifact inside it.
        assert_eq!(
            fingerprint_artifacts(&target, std::slice::from_ref(&target)),
            Err("unsafe cached Rust artifact: ".into())
        );
        assert!(!artifacts_match(&target, &[]));
        let reversed = fingerprints.iter().rev().cloned().collect::<Vec<_>>();
        assert!(!artifacts_match(&target, &reversed));
        let mut wrong = fingerprints.clone();
        wrong[0].bytes += 1;
        assert!(!artifacts_match(&target, &wrong));
        let mut changed = fingerprints.clone();
        changed[0].sha256 = "0".repeat(64);
        assert!(!artifacts_match(&target, &changed));
        let mut unsafe_path = fingerprints.clone();
        unsafe_path[0].path = "../outside".into();
        assert!(!artifacts_match(&target, &unsafe_path));
        let mut directory = fingerprints;
        directory[0].path = "debug".into();
        assert!(!artifacts_match(&target, &directory));
    }

    #[test]
    fn cache_requires_exact_key_regular_workspace_target_and_sorted_sources() {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-cache-{}-{nonce}",
            std::process::id()
        ));
        let workspace = root.join(".supercov/cache/workspace/project");
        let target = rust_target_directory(&root);
        fs::create_dir_all(workspace.join(".supercov")).unwrap();
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::create_dir_all(workspace.join("tests")).unwrap();
        fs::write(workspace.join("src/lib.rs"), "fn work() {}\n").unwrap();
        fs::write(workspace.join("tests/test.rs"), "#[test] fn works() {}\n").unwrap();
        fs::create_dir_all(target.join("debug")).unwrap();
        fs::write(target.join("debug/test-bin"), b"binary").unwrap();
        let manifest = CoverageManifest {
            unmeasured: Vec::new(),
            decisions: Vec::new(),
            points: vec![PointMeta {
                id: "rs:statement:000000000000000000000000".into(),
                kind: PointKind::Statement,
                file: "src/lib.rs".into(),
                line: 1,
                column: 0,
                source: "work();".into(),
                label: None,
            }],
            branches: Vec::new(),
            limitations: Vec::new(),
            scope: None,
            assertion_sites: Vec::new(),
        };
        let written = write_rust_build_cache(
            &root,
            &workspace,
            "key",
            "time",
            &["tests/test.rs".into(), "src/lib.rs".into()],
            &manifest,
            &crate::rust_instrumenter::RustProbeLayout::default(),
            &[target.join("debug/test-bin")],
        )
        .unwrap();
        assert_eq!(written.source_files, ["src/lib.rs", "tests/test.rs"]);
        assert_eq!(
            read_rust_build_cache(&workspace, &target, "key"),
            Some(written.clone())
        );
        assert!(read_rust_build_cache(&workspace, &target, "other").is_none());
        fs::write(workspace.join("src/lib.rs"), "fn changed() {}\n").unwrap();
        assert!(read_rust_build_cache(&workspace, &target, "key").is_none());
        fs::write(workspace.join("src/lib.rs"), "fn work() {}\n").unwrap();
        assert!(read_rust_build_cache(&workspace, &target, "key").is_some());
        // A record that cannot be read is no cache, and one whose artifacts
        // cannot be fingerprinted is not written.
        assert!(
            crate::fault::during("fs.read", || read_rust_build_cache(
                &workspace, &target, "key"
            ))
            .is_none()
        );
        assert!(
            crate::fault::during("fs.canonicalize", || {
                write_rust_build_cache(
                    &root,
                    &workspace,
                    "key",
                    "time",
                    &["src/lib.rs".into()],
                    &manifest,
                    &crate::rust_instrumenter::RustProbeLayout::default(),
                    &[target.join("debug/test-bin")],
                )
            })
            .is_err()
        );
        // A record of another schema, with no sources, no points or its
        // sources out of order is no cache.
        let tampered = |change: &dyn Fn(&mut RustBuildCacheMetadata)| {
            let mut metadata = written.clone();
            change(&mut metadata);
            fs::write(
                workspace.join(CACHE_FILE),
                serde_json::to_vec(&metadata).unwrap(),
            )
            .unwrap();
            read_rust_build_cache(&workspace, &target, "key")
        };
        assert!(tampered(&|metadata| metadata.schema_version += 1).is_none());
        assert!(tampered(&|metadata| metadata.source_files.clear()).is_none());
        assert!(tampered(&|metadata| metadata.manifest.points.clear()).is_none());
        assert!(tampered(&|metadata| metadata.source_files.reverse()).is_none());
        assert!(tampered(&|_| {}).is_some());
        // Nor is one whose built artifact has changed since.
        fs::write(target.join("debug/test-bin"), b"rebuilt").unwrap();
        assert!(tampered(&|_| {}).is_none());
        fs::write(target.join("debug/test-bin"), b"binary").unwrap();
        // A record that is no JSON is no cache, and a source that cannot be
        // read is not cached at all.
        fs::write(workspace.join(CACHE_FILE), "{").unwrap();
        assert!(read_rust_build_cache(&workspace, &target, "key").is_none());
        assert_eq!(
            write_rust_build_cache(
                &root,
                &workspace,
                "key",
                "time",
                &["src/missing.rs".into()],
                &manifest,
                &crate::rust_instrumenter::RustProbeLayout::default(),
                &[],
            ),
            Err("could not authenticate instrumented Rust sources".into())
        );
        fs::write(target.join("debug/test-bin"), b"tamper").unwrap();
        assert!(read_rust_build_cache(&workspace, &target, "key").is_none());
        fs::remove_dir_all(target).unwrap();
        assert!(read_rust_build_cache(&workspace, &root.join("missing"), "key").is_none());
        fs::remove_dir_all(root).unwrap();
    }
}
