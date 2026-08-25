//! JavaScript source frontend for Rust-owned executions.
//!
//! The frontend mutates only an already-isolated workspace. JavaScript files
//! are transformed by the Rust instrumenter; the small Node/browser runtime
//! remains a language shim and is copied into the workspace under `.supercov`.

use std::{
    collections::BTreeMap,
    fs::{self, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::{
    js_instrumenter::{
        CandidateBranch, CandidateDecision, CandidateError, CandidateLimitation, CandidatePoint,
        instrument_direct_candidate, instrument_node_assertion_phases,
    },
    project_discovery::CoverageProject,
    source_discovery::{SourceLimitation, SourceScope},
};

const RUNTIME_INSTANCE_MARKER: &str = "__SUPERCOV_RUNTIME_INSTANCE__";
const RUNTIME_FILES: &[&str] = &[
    "atomic.js",
    "launchSupervisor.js",
    "nodeAssert.js",
    "nodeAssertAdapter.js",
    "nodeAssertStrict.js",
    "nodeTest.js",
    "playwright.js",
    "playwrightReporter.js",
    "provenance.js",
    "register.mjs",
    "resolve-loader.mjs",
    "runnerEvidence.js",
    "runtime.js",
    "transport.js",
    "types.js",
];
static UNIQUE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
pub enum JavascriptFrontendError {
    Io {
        path: PathBuf,
        source: io::Error,
    },
    Instrument {
        file: String,
        source: CandidateError,
    },
    MissingRuntimeMarker,
    Serialize(serde_json::Error),
    UnsafeSourcePath(String),
}

impl std::fmt::Display for JavascriptFrontendError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, source } => write!(formatter, "{}: {source}", path.display()),
            Self::Instrument { file, source } => {
                write!(formatter, "failed to instrument {file}: {source:?}")
            }
            Self::MissingRuntimeMarker => write!(
                formatter,
                "generated Supercov runtime is missing its instance marker"
            ),
            Self::Serialize(error) => write!(formatter, "failed to serialize manifest: {error}"),
            Self::UnsafeSourcePath(file) => write!(formatter, "unsafe source path: {file}"),
        }
    }
}

impl std::error::Error for JavascriptFrontendError {}

fn io_error(path: &Path, source: io::Error) -> JavascriptFrontendError {
    JavascriptFrontendError::Io {
        path: path.to_owned(),
        source,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct JavascriptManifest {
    pub decisions: Vec<CandidateDecision>,
    pub points: Vec<CandidatePoint>,
    pub branches: Vec<CandidateBranch>,
    pub limitations: Vec<CandidateLimitation>,
    pub scope: SourceScope,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedJavascriptFrontend {
    pub manifest: JavascriptManifest,
    pub manifest_path: PathBuf,
    pub preload_path: PathBuf,
    pub assertion_calls: usize,
}

pub fn javascript_runtime_files(runtime_root: &Path) -> Vec<PathBuf> {
    RUNTIME_FILES
        .iter()
        .map(|name| runtime_root.join(name))
        .collect()
}

fn unique() -> String {
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

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), JavascriptFrontendError> {
    let parent = path
        .parent()
        .ok_or_else(|| JavascriptFrontendError::UnsafeSourcePath(path.display().to_string()))?;
    fs::create_dir_all(parent).map_err(|source| io_error(parent, source))?;
    let temporary = parent.join(format!(".supercov-write-{}", unique()));
    let result = (|| {
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
            .map_err(|source| io_error(&temporary, source))?;
        output
            .write_all(contents)
            .and_then(|_| output.sync_all())
            .map_err(|source| io_error(&temporary, source))?;
        fs::rename(&temporary, path).map_err(|source| io_error(path, source))?;
        OpenOptions::new()
            .read(true)
            .open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| io_error(parent, source))
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn checked_source_path(workspace: &Path, file: &str) -> Result<PathBuf, JavascriptFrontendError> {
    let relative = Path::new(file);
    if relative.is_absolute()
        || relative
            .components()
            .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(JavascriptFrontendError::UnsafeSourcePath(file.to_owned()));
    }
    Ok(workspace.join(relative))
}

fn isolate_runtime(source: &str, collector_id: &str) -> Result<String, JavascriptFrontendError> {
    let double = format!("runtimeInstanceToken = \"{RUNTIME_INSTANCE_MARKER}\"");
    let single = format!("runtimeInstanceToken = '{RUNTIME_INSTANCE_MARKER}'");
    if let Some(index) = source.find(&double) {
        let mut isolated = source.to_owned();
        isolated.replace_range(
            index..index + double.len(),
            &format!("runtimeInstanceToken = \"{collector_id}\""),
        );
        return Ok(isolated);
    }
    if let Some(index) = source.find(&single) {
        let mut isolated = source.to_owned();
        isolated.replace_range(
            index..index + single.len(),
            &format!("runtimeInstanceToken = '{collector_id}'"),
        );
        return Ok(isolated);
    }
    Err(JavascriptFrontendError::MissingRuntimeMarker)
}

fn copy_runtime(
    runtime_root: &Path,
    generated: &Path,
    collector_id: &str,
) -> Result<(), JavascriptFrontendError> {
    fs::create_dir_all(generated).map_err(|source| io_error(generated, source))?;
    atomic_write(
        &generated.join("package.json"),
        b"{\"private\":true,\"type\":\"module\"}\n",
    )?;
    for name in RUNTIME_FILES {
        let source_path = runtime_root.join(name);
        let destination = generated.join(name);
        let bytes = fs::read(&source_path).map_err(|source| io_error(&source_path, source))?;
        if *name == "runtime.js" {
            let text = String::from_utf8(bytes).map_err(|source| {
                io_error(
                    &source_path,
                    io::Error::new(io::ErrorKind::InvalidData, source),
                )
            })?;
            atomic_write(
                &destination,
                isolate_runtime(&text, collector_id)?.as_bytes(),
            )?;
        } else {
            atomic_write(&destination, &bytes)?;
        }
    }
    Ok(())
}

fn limitation_from_source(value: &SourceLimitation) -> CandidateLimitation {
    CandidateLimitation {
        id: value.id.clone(),
        kind: value.kind.clone(),
        file: value.file.clone(),
        line: value.line,
        column: value.column,
        source: value.source.clone(),
        reason: value.reason.clone(),
    }
}

/// Prepare the complete JavaScript frontend inside an isolated workspace.
/// The source project is read only through the copied workspace inventory.
pub fn prepare_javascript_frontend(
    workspace: &Path,
    project: &CoverageProject,
    runtime_root: &Path,
    collector_id: &str,
) -> Result<PreparedJavascriptFrontend, JavascriptFrontendError> {
    let generated = workspace.join(".supercov");
    copy_runtime(runtime_root, &generated, collector_id)?;

    let mut decisions = BTreeMap::new();
    let mut points = BTreeMap::new();
    let mut branches = BTreeMap::new();
    let mut limitations = BTreeMap::new();
    for limitation in &project.source_limitations {
        limitations.insert(limitation.id.clone(), limitation_from_source(limitation));
    }

    for file in &project.source_files {
        let path = checked_source_path(workspace, file)?;
        let source = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
        let output = instrument_direct_candidate(&source, file).map_err(|source| {
            JavascriptFrontendError::Instrument {
                file: file.clone(),
                source,
            }
        })?;
        atomic_write(&path, output.code.as_bytes())?;
        for value in output.decisions {
            decisions.insert(value.id.clone(), value);
        }
        for value in output.points {
            points.insert(value.id.clone(), value);
        }
        for value in output.branches {
            branches.insert(value.id.clone(), value);
        }
        for value in output.coverage_limitations {
            limitations.insert(value.id.clone(), value);
        }
    }

    let mut assertion_calls = 0;
    for entry in &project.source_scope.entries {
        let path = checked_source_path(workspace, &entry.file)?;
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let output = instrument_node_assertion_phases(&source, &entry.file).map_err(|source| {
            JavascriptFrontendError::Instrument {
                file: entry.file.clone(),
                source,
            }
        })?;
        if output.assertions > 0 {
            atomic_write(&path, output.code.as_bytes())?;
            assertion_calls += output.assertions;
        }
    }

    let mut manifest = JavascriptManifest {
        decisions: decisions.into_values().collect(),
        points: points.into_values().collect(),
        branches: branches.into_values().collect(),
        limitations: limitations.into_values().collect(),
        scope: project.source_scope.clone(),
    };
    manifest.decisions.sort_by_key(|value| {
        (
            value.file.clone(),
            value.line,
            value.column,
            value.id.clone(),
        )
    });
    manifest.points.sort_by_key(|value| {
        (
            value.file.clone(),
            value.line,
            value.column,
            value.id.clone(),
        )
    });
    manifest.branches.sort_by_key(|value| {
        (
            value.file.clone(),
            value.line,
            value.column,
            value.id.clone(),
        )
    });
    manifest.limitations.sort_by_key(|value| {
        (
            value.file.clone(),
            value.line,
            value.column,
            value.id.clone(),
        )
    });

    let manifest_path = generated.join("manifest.json");
    let mut encoded =
        serde_json::to_vec_pretty(&manifest).map_err(JavascriptFrontendError::Serialize)?;
    encoded.push(b'\n');
    atomic_write(&manifest_path, &encoded)?;
    atomic_write(
        &generated.join("instrumentation-complete"),
        b"coverage-completeness-v2\n",
    )?;
    Ok(PreparedJavascriptFrontend {
        manifest,
        manifest_path,
        preload_path: generated.join("register.mjs"),
        assertion_calls,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::project_discovery::discover_coverage_project;

    fn temporary(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("supercov-js-frontend-{name}-{}", unique()))
    }

    #[test]
    fn runtime_isolation_replaces_only_the_assignment_marker() {
        let source = concat!(
            "const runtimeInstanceToken = \"__SUPERCOV_RUNTIME_INSTANCE__\";\n",
            "const selected = runtimeInstanceToken === \"__SUPERCOV_\" + \"RUNTIME_INSTANCE__\";\n"
        );
        let isolated = isolate_runtime(source, "collector-123").unwrap();
        assert!(isolated.contains("runtimeInstanceToken = \"collector-123\""));
        assert!(isolated.contains("=== \"__SUPERCOV_\" + \"RUNTIME_INSTANCE__\""));
    }

    #[test]
    fn prepares_sorted_complete_manifest_without_touching_source_project() {
        let source_root = temporary("source");
        let workspace = temporary("workspace");
        let runtime = temporary("runtime");
        fs::create_dir_all(source_root.join("src")).unwrap();
        fs::write(
            source_root.join("src/example.mjs"),
            "export function value(a, b) { if (a || b) return 1; return 0; }\n",
        )
        .unwrap();
        fs::write(source_root.join("package.json"), "{\"type\":\"module\"}\n").unwrap();
        fs::create_dir_all(workspace.join("src")).unwrap();
        fs::copy(
            source_root.join("src/example.mjs"),
            workspace.join("src/example.mjs"),
        )
        .unwrap();
        for name in RUNTIME_FILES {
            fs::create_dir_all(&runtime).unwrap();
            let contents = if *name == "runtime.js" {
                "const runtimeInstanceToken = \"__SUPERCOV_RUNTIME_INSTANCE__\";\n"
            } else {
                "export {};\n"
            };
            fs::write(runtime.join(name), contents).unwrap();
        }
        let project = discover_coverage_project(
            &source_root,
            &BTreeMap::new(),
            &["node".into(), "--test".into()],
        )
        .unwrap();
        let original = fs::read_to_string(source_root.join("src/example.mjs")).unwrap();
        let prepared =
            prepare_javascript_frontend(&workspace, &project, &runtime, "collector-test").unwrap();
        assert_eq!(
            fs::read_to_string(source_root.join("src/example.mjs")).unwrap(),
            original
        );
        let transformed = fs::read_to_string(workspace.join("src/example.mjs")).unwrap();
        assert!(transformed.contains("__SUPERCOV_DIRECT_RUNTIME__"));
        assert_eq!(prepared.manifest.decisions.len(), 1);
        assert!(!prepared.manifest.points.is_empty());
        assert_eq!(prepared.manifest.scope, project.source_scope);
        assert!(prepared.manifest_path.is_file());
        assert!(prepared.preload_path.is_file());
        assert_eq!(prepared.assertion_calls, 0);
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
        fs::remove_dir_all(runtime).unwrap();
    }
}
