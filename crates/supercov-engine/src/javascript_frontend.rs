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
use sha2::{Digest, Sha256};

use crate::{
    js_instrumenter::{
        CandidateBranch, CandidateDecision, CandidateError, CandidateLimitation, CandidatePoint,
        instrument_candidate, instrument_direct_candidate,
        instrument_node_assertion_phases_with_expect_modules,
    },
    project_discovery::{BuildAdapter, CoverageProject},
    source_discovery::{SourceLimitation, SourceScope},
};

const RUNTIME_INSTANCE_MARKER: &str = "__SUPERCOV_RUNTIME_INSTANCE__";
const RUNTIME_FILES: &[&str] = &[
    "atomic.js",
    "esmInterceptor.js",
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
    "vitest.js",
    "vitestReporter.js",
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
    pub playwright_config_path: PathBuf,
    pub vite_config_path: PathBuf,
    pub vitest_config_path: PathBuf,
    pub assertion_calls: usize,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ViteTransform {
    source_sha256: String,
    code: String,
    map: Option<serde_json::Value>,
}

pub fn javascript_runtime_files(runtime_root: &Path) -> Vec<PathBuf> {
    RUNTIME_FILES
        .iter()
        .map(|name| runtime_root.join(name))
        .collect()
}

fn embedded_runtime(name: &str) -> Option<&'static [u8]> {
    match name {
        "atomic.js" => Some(include_bytes!("../runtime/atomic.js")),
        "esmInterceptor.js" => Some(include_bytes!("../runtime/esmInterceptor.js")),
        "launchSupervisor.js" => Some(include_bytes!("../runtime/launchSupervisor.js")),
        "nodeAssert.js" => Some(include_bytes!("../runtime/nodeAssert.js")),
        "nodeAssertAdapter.js" => Some(include_bytes!("../runtime/nodeAssertAdapter.js")),
        "nodeAssertStrict.js" => Some(include_bytes!("../runtime/nodeAssertStrict.js")),
        "nodeTest.js" => Some(include_bytes!("../runtime/nodeTest.js")),
        "playwright.js" => Some(include_bytes!("../runtime/playwright.js")),
        "playwrightReporter.js" => Some(include_bytes!("../runtime/playwrightReporter.js")),
        "provenance.js" => Some(include_bytes!("../runtime/provenance.js")),
        "register.mjs" => Some(include_bytes!("../runtime/register.mjs")),
        "resolve-loader.mjs" => Some(include_bytes!("../runtime/resolve-loader.mjs")),
        "runnerEvidence.js" => Some(include_bytes!("../runtime/runnerEvidence.js")),
        "runtime.js" => Some(include_bytes!("../runtime/runtime.js")),
        "transport.js" => Some(include_bytes!("../runtime/transport.js")),
        "types.js" => Some(include_bytes!("../runtime/types.js")),
        "vitest.js" => Some(include_bytes!("../runtime/vitest.js")),
        "vitestReporter.js" => Some(include_bytes!("../runtime/vitestReporter.js")),
        _ => None,
    }
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

#[cfg(not(windows))]
fn create_directory_all(path: &Path) -> Result<(), JavascriptFrontendError> {
    fs::create_dir_all(path).map_err(|source| io_error(path, source))
}

#[cfg(windows)]
fn create_directory_all(path: &Path) -> Result<(), JavascriptFrontendError> {
    let is_plain_directory = |candidate: &Path| {
        fs::symlink_metadata(candidate)
            .is_ok_and(|metadata| metadata.is_dir() && !metadata.file_type().is_symlink())
    };
    if is_plain_directory(path) {
        return Ok(());
    }
    if let Some(parent) = path.parent()
        && !parent.is_dir()
    {
        create_directory_all(parent)?;
    }

    const ATTEMPTS: usize = 11;
    for attempt in 0..ATTEMPTS {
        // Windows' recursive `create_dir_all` can report AccessDenied while
        // traversing an existing 8.3 short-name temp path. Create only the
        // exact owned leaf after its parent exists, and accept the result only
        // when that leaf is a real directory rather than a link.
        if is_plain_directory(path) {
            return Ok(());
        }
        match fs::create_dir(path) {
            Ok(()) => return Ok(()),
            Err(source)
                if source.kind() == io::ErrorKind::PermissionDenied && attempt + 1 < ATTEMPTS =>
            {
                // Windows scanners and just-closed directory handles can
                // transiently reject creation of a brand-new path. Retry the
                // exact owned path; never broaden or redirect the target.
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(source) => return Err(io_error(path, source)),
        }
    }
    unreachable!("the final directory-creation attempt always returns")
}

fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), JavascriptFrontendError> {
    let parent = path
        .parent()
        .ok_or_else(|| JavascriptFrontendError::UnsafeSourcePath(path.display().to_string()))?;
    create_directory_all(parent)?;
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

/// The generated language shims are embedded without their TypeScript sources.
/// Keeping a trailing source-map directive would make Node and browser tooling
/// look for files that intentionally are not part of the runtime distribution.
fn strip_source_map_reference(mut bytes: Vec<u8>) -> Vec<u8> {
    const MARKER: &[u8] = b"\n//# sourceMappingURL=";
    if let Some(index) = bytes
        .windows(MARKER.len())
        .rposition(|window| window == MARKER)
    {
        let suffix = &bytes[index + MARKER.len()..];
        let suffix = suffix.strip_suffix(b"\n").unwrap_or(suffix);
        let suffix = suffix.strip_suffix(b"\r").unwrap_or(suffix);
        if !suffix.contains(&b'\n') && !suffix.contains(&b'\r') {
            bytes.truncate(index + 1);
        }
    }
    bytes
}

fn copy_runtime(
    runtime_root: Option<&Path>,
    generated: &Path,
    collector_id: &str,
) -> Result<(), JavascriptFrontendError> {
    create_directory_all(generated)?;
    atomic_write(
        &generated.join("package.json"),
        b"{\"private\":true,\"type\":\"module\"}\n",
    )?;
    for name in RUNTIME_FILES {
        let destination = generated.join(name);
        let (bytes, source_path) = if let Some(runtime_root) = runtime_root {
            let source_path = runtime_root.join(name);
            let bytes = fs::read(&source_path).map_err(|source| io_error(&source_path, source))?;
            (bytes, source_path)
        } else {
            let bytes = embedded_runtime(name)
                .expect("every declared runtime file must have an embedded asset")
                .to_vec();
            (bytes, PathBuf::from(format!("embedded:{name}")))
        };
        let bytes = strip_source_map_reference(bytes);
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
            atomic_write(
                &generated.join("applicationRuntime.js"),
                isolate_runtime(&text, &format!("{collector_id}-application"))?.as_bytes(),
            )?;
        } else {
            atomic_write(&destination, &bytes)?;
        }
    }
    atomic_write(
        &generated.join("runtime.d.ts"),
        b"export declare function coverageHit(...args: any[]): any;\n\
export declare function selectionBegin(...args: any[]): any;\n\
export declare function selectionRight(...args: any[]): any;\n\
export declare function selectionEnd(...args: any[]): any;\n\
export declare function optionalSelect(...args: any[]): any;\n\
export declare function optionalCallBegin(...args: any[]): any;\n\
export declare function optionalCallReached(...args: any[]): any;\n\
export declare function optionalCallContinued(...args: any[]): any;\n\
export declare function optionalCallEnd(...args: any[]): any;\n\
export declare function defaultSelected(...args: any[]): any;\n\
export declare function defaultEntered(...args: any[]): any;\n\
export declare function tryBegin(...args: any[]): any;\n\
export declare function tryCatch(...args: any[]): any;\n\
export declare function tryEnd(...args: any[]): any;\n\
export declare function loopBegin(...args: any[]): any;\n\
export declare function loopEntered(...args: any[]): any;\n\
export declare function loopEnd(...args: any[]): any;\n\
export declare function mcdcBegin(...args: any[]): any;\n\
export declare function mcdcCondition(...args: any[]): any;\n\
export declare function mcdcEnd(...args: any[]): any;\n\
export declare function registerProbeV2(...args: any[]): any;\n\
export declare function coverageHitV2(...args: any[]): any;\n\
export declare function mcdcEndV2(...args: any[]): any;\n",
    )?;
    Ok(())
}

fn generic_runtime_binding(
    workspace: &Path,
    project: &CoverageProject,
    source_path: &Path,
    generated: &Path,
) -> Result<String, JavascriptFrontendError> {
    let mut hosts = project
        .source_roots
        .iter()
        .filter_map(|root| {
            let candidate = workspace.join(root);
            if candidate.is_dir() && source_path.strip_prefix(&candidate).is_ok() {
                Some(candidate)
            } else if candidate.is_file() && candidate == source_path {
                candidate.parent().map(Path::to_owned)
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    hosts.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    let host = hosts
        .into_iter()
        .next()
        .unwrap_or_else(|| workspace.to_owned());
    let runtime_directory = host.join(".supercov");
    fs::create_dir_all(&runtime_directory)
        .map_err(|source| io_error(&runtime_directory, source))?;
    for name in ["runtime.js", "runtime.d.ts"] {
        let source = generated.join(name);
        let destination = runtime_directory.join(name);
        let contents = fs::read(&source).map_err(|error| io_error(&source, error))?;
        atomic_write(&destination, &contents)?;
    }
    let parent = source_path.parent().ok_or_else(|| {
        JavascriptFrontendError::UnsafeSourcePath(source_path.display().to_string())
    })?;
    let local = parent.strip_prefix(&host).map_err(|_| {
        JavascriptFrontendError::UnsafeSourcePath(source_path.display().to_string())
    })?;
    let depth = local.components().count();
    Ok(if depth == 0 {
        "./.supercov/runtime.js".into()
    } else {
        format!("{}.supercov/runtime.js", "../".repeat(depth))
    })
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

fn relocated_project_file(
    workspace: &Path,
    project: &CoverageProject,
    source: Option<&PathBuf>,
) -> Option<PathBuf> {
    let source = source?;
    let relative = source.strip_prefix(&project.root).ok()?;
    Some(workspace.join(relative))
}

fn write_vitest_config(
    workspace: &Path,
    project: &CoverageProject,
    generated: &Path,
) -> Result<PathBuf, JavascriptFrontendError> {
    let path = generated.join("vitest.config.mjs");
    let original = relocated_project_file(workspace, project, project.vitest_config.as_ref())
        .map(|path| path.display().to_string());
    let original = serde_json::to_string(&original).map_err(JavascriptFrontendError::Serialize)?;
    let source = format!(
        "import * as viteNamespace from 'vite';\n\
         import {{ resolve }} from 'node:path';\n\
         import SupercovVitestReporter from './vitestReporter.js';\n\
         import {{ supercovViteInstrumentation }} from './viteInstrumentation.mjs';\n\
         const vite = viteNamespace.default ?? viteNamespace;\n\
         const {{ loadConfigFromFile, mergeConfig }} = vite;\n\
         const discoveredConfig = {original};\n\
         export default async function supercovVitestConfig(env) {{\n\
           const originalPath = process.env.SUPERCOV_ORIGINAL_VITEST_CONFIG || discoveredConfig;\n\
           const loaded = originalPath ? await loadConfigFromFile(env, originalPath, process.cwd()) : undefined;\n\
           const config = mergeConfig(loaded?.config ?? {{}}, {{\n\
             cacheDir: resolve(process.cwd(), '.supercov/vitest-cache'),\n\
             plugins: [supercovViteInstrumentation(process.cwd())],\n\
             test: {{ setupFiles: [resolve(process.cwd(), '.supercov/vitest.js')], maxConcurrency: 1 }},\n\
           }});\n\
           const configuredReporters = loaded?.config?.test?.reporters;\n\
           config.test ??= {{}};\n\
           config.test.reporters = configuredReporters\n\
             ? [...(Array.isArray(configuredReporters) ? configuredReporters : [configuredReporters]), new SupercovVitestReporter()]\n\
             : ['default', new SupercovVitestReporter()];\n\
           return config;\n\
         }}\n"
    );
    atomic_write(&path, source.as_bytes())?;
    Ok(path)
}

fn configure_playwright_runtime(
    generated: &Path,
    project: &CoverageProject,
) -> Result<(), JavascriptFrontendError> {
    let adapter_path = generated.join("playwright.js");
    let mut adapter =
        fs::read_to_string(&adapter_path).map_err(|source| io_error(&adapter_path, source))?;
    adapter = adapter
        .replace("__SUPERCOV_PLAYWRIGHT_MODULE__", &project.playwright_module)
        .replace(
            "__SUPERCOV_PLAYWRIGHT_TEST_EXPORT__",
            &project.playwright_test_export,
        );
    let mut exports = Vec::new();
    if project.playwright_test_export != "test" {
        exports.push(format!(
            "export {{ instrumentedTest as {} }};",
            project.playwright_test_export
        ));
    }
    exports.extend(
        project
            .playwright_exports
            .iter()
            .filter(|name| {
                name.as_str() != "test"
                    && name.as_str() != "expect"
                    && *name != &project.playwright_test_export
            })
            .map(|name| {
                let encoded = serde_json::to_string(name)
                    .expect("serializing a JavaScript export name cannot fail");
                format!("export const {name} = adapter[{encoded}];")
            }),
    );
    adapter = adapter.replace("/*__SUPERCOV_ADAPTER_EXPORTS__*/", &exports.join("\n"));
    atomic_write(&adapter_path, adapter.as_bytes())?;

    let loader_path = generated.join("resolve-loader.mjs");
    let loader = fs::read_to_string(&loader_path)
        .map_err(|source| io_error(&loader_path, source))?
        .replace("__SUPERCOV_PLAYWRIGHT_MODULE__", &project.playwright_module);
    atomic_write(&loader_path, loader.as_bytes())
}

fn write_playwright_config(
    workspace: &Path,
    project: &CoverageProject,
    generated: &Path,
) -> Result<PathBuf, JavascriptFrontendError> {
    let path = generated.join("playwright.config.mjs");
    let original = relocated_project_file(workspace, project, project.playwright_config.as_ref());
    let original_import = if let Some(original) = &original {
        let relative = original
            .strip_prefix(workspace)
            .map_err(|_| JavascriptFrontendError::UnsafeSourcePath(original.display().to_string()))?
            .to_string_lossy()
            .replace('\\', "/");
        let specifier = serde_json::to_string(&format!("../{relative}"))
            .map_err(JavascriptFrontendError::Serialize)?;
        format!("import original from {specifier};\n")
    } else {
        "const original = {};\n".into()
    };
    let source = format!(
        "import './register.mjs';\n\
         import {{ dirname, isAbsolute, relative, resolve }} from 'node:path';\n\
         import {{ fileURLToPath }} from 'node:url';\n\
         {original_import}\
         const resolvedValue = typeof original === 'function' ? await original({{ command: 'test', mode: 'test' }}) : original;\n\
         const resolved = resolvedValue ?? {{}};\n\
         const runtimeProjectRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..');\n\
         const originalDirectory = {};
         const sourceProjectRoot = process.env.SUPERCOV_SOURCE_PROJECT_ROOT;\n\
         const runtimePath = value => {{\n\
           if (!value) return value;\n\
           const absolute = isAbsolute(value) ? value : resolve(originalDirectory, value);\n\
           const local = relative(runtimeProjectRoot, absolute);\n\
           if (local === '' || (!local.startsWith('..') && !isAbsolute(local))) return absolute;\n\
           if (sourceProjectRoot) {{\n\
             const sourceLocal = relative(sourceProjectRoot, absolute);\n\
             if (sourceLocal === '' || (!sourceLocal.startsWith('..') && !isAbsolute(sourceLocal))) return resolve(runtimeProjectRoot, sourceLocal);\n\
           }}\n\
           throw new Error('Supercov refuses a Playwright output/cwd outside the isolated project: ' + absolute);\n\
         }};\n\
         const normalizeWebServer = server => server ? ({{ ...server, cwd: runtimePath(server.cwd ?? originalDirectory) }}) : server;\n\
         const normalized = {{ ...resolved,\n\
           testDir: runtimePath(resolved.testDir),\n\
           outputDir: runtimePath(resolved.outputDir),\n\
           snapshotDir: runtimePath(resolved.snapshotDir),\n\
           projects: resolved.projects?.map(project => ({{ ...project, testDir: runtimePath(project.testDir), outputDir: runtimePath(project.outputDir), snapshotDir: runtimePath(project.snapshotDir) }})),\n\
           webServer: Array.isArray(resolved.webServer) ? resolved.webServer.map(normalizeWebServer) : normalizeWebServer(resolved.webServer),\n\
         }};\n\
         const configuredReporters = normalized.reporter;\n\
         const reporters = configuredReporters\n\
           ? (typeof configuredReporters === 'string' ? [[configuredReporters]] : (Array.isArray(configuredReporters[0]) ? configuredReporters : [configuredReporters]))\n\
           : [['list']];\n\
         const coverageReporter = resolve(runtimeProjectRoot, '.supercov/playwrightReporter.js');\n\
         export default {{ ...normalized, reporter: [...reporters, [coverageReporter]] }};\n",
        serde_json::to_string(
            &original
                .as_ref()
                .and_then(|path| path.parent())
                .unwrap_or(workspace)
                .display()
                .to_string()
        )
        .map_err(JavascriptFrontendError::Serialize)?
    );
    atomic_write(&path, source.as_bytes())?;
    Ok(path)
}

fn write_vite_config(
    workspace: &Path,
    generated: &Path,
) -> Result<PathBuf, JavascriptFrontendError> {
    let path = generated.join("vite.config.mjs");
    let workspace = serde_json::to_string(&workspace.display().to_string())
        .map_err(JavascriptFrontendError::Serialize)?;
    let source = format!(
        "import * as viteNamespace from 'vite';\n\
         import {{ isAbsolute, relative, resolve }} from 'node:path';\n\
         import {{ supercovViteInstrumentation }} from './viteInstrumentation.mjs';\n\
         const vite = viteNamespace.default ?? viteNamespace;\n\
         const {{ loadConfigFromFile, mergeConfig }} = vite;\n\
         export default async function supercovViteConfig(env) {{\n\
           const isolatedRoot = {workspace};\n\
           const loaded = await loadConfigFromFile(env, undefined, isolatedRoot);\n\
           const config = loaded?.config ?? {{}};\n\
           const relocate = (value, label) => {{\n\
             const absolute = isAbsolute(value) ? value : resolve(isolatedRoot, value);\n\
             const local = relative(isolatedRoot, absolute);\n\
             if (local === '' || (!local.startsWith('..') && !isAbsolute(local))) return absolute;\n\
             throw new Error('Supercov refuses ' + label + ' outside the isolated project: ' + absolute);\n\
           }};\n\
           const relocateOutput = output => output ? ({{ ...output, dir: output.dir ? relocate(output.dir, 'Rollup output') : output.dir, file: output.file ? relocate(output.file, 'Rollup output') : output.file }}) : output;\n\
           const rollupOutput = config.build?.rollupOptions?.output;\n\
           const safe = {{ ...config,\n\
             cacheDir: resolve(isolatedRoot, '.supercov/vite-cache'),\n\
             build: {{ ...config.build, outDir: relocate(config.build?.outDir ?? 'dist', 'Vite build output'), rollupOptions: {{ ...config.build?.rollupOptions, output: Array.isArray(rollupOutput) ? rollupOutput.map(relocateOutput) : relocateOutput(rollupOutput) }} }},\n\
           }};\n\
           return mergeConfig(safe, {{ plugins: [supercovViteInstrumentation(isolatedRoot)] }});\n\
         }}\n"
    );
    atomic_write(&path, source.as_bytes())?;
    Ok(path)
}

fn write_vite_transforms(
    generated: &Path,
    transforms: &BTreeMap<String, ViteTransform>,
) -> Result<(), JavascriptFrontendError> {
    let mut payload = serde_json::to_vec(transforms).map_err(JavascriptFrontendError::Serialize)?;
    payload.push(b'\n');
    atomic_write(&generated.join("vite-transforms.json"), &payload)?;
    let adapter = "import { createHash } from 'node:crypto';\n\
import { readFileSync } from 'node:fs';\n\
import { relative, resolve, sep } from 'node:path';\n\
const transforms = JSON.parse(readFileSync(new URL('./vite-transforms.json', import.meta.url), 'utf8'));\n\
const sha256 = value => createHash('sha256').update(value).digest('hex');\n\
export function supercovViteInstrumentation(root) {\n\
  const runtimePath = resolve(root, '.supercov/applicationRuntime.js');\n\
  return {\n\
    name: 'supercov-rust-instrumentation',\n\
    enforce: 'pre',\n\
    resolveId(id) { return id === 'virtual:supercov-runtime' ? runtimePath : null; },\n\
    transform(code, rawId) {\n\
      const id = rawId.split('?')[0] ?? rawId;\n\
      const local = relative(root, id).split(sep).join('/');\n\
      const transformed = transforms[local];\n\
      if (!transformed) return null;\n\
      if (sha256(code) !== transformed.sourceSha256)\n\
        throw new Error('Supercov source changed before Rust instrumentation: ' + local);\n\
      return { code: transformed.code, map: transformed.map ?? null };\n\
    },\n\
  };\n\
}\n";
    atomic_write(
        &generated.join("viteInstrumentation.mjs"),
        adapter.as_bytes(),
    )
}

/// Prepare the complete JavaScript frontend inside an isolated workspace.
/// The source project is read only through the copied workspace inventory.
pub fn prepare_javascript_frontend(
    workspace: &Path,
    project: &CoverageProject,
    runtime_root: Option<&Path>,
    collector_id: &str,
) -> Result<PreparedJavascriptFrontend, JavascriptFrontendError> {
    let generated = workspace.join(".supercov");
    copy_runtime(runtime_root, &generated, collector_id)?;
    configure_playwright_runtime(&generated, project)?;
    let playwright_config_path = write_playwright_config(workspace, project, &generated)?;
    let vite_config_path = write_vite_config(workspace, &generated)?;
    let vitest_config_path = write_vitest_config(workspace, project, &generated)?;

    let mut decisions = BTreeMap::new();
    let mut points = BTreeMap::new();
    let mut branches = BTreeMap::new();
    let mut limitations = BTreeMap::new();
    let mut vite_transforms = BTreeMap::new();
    for limitation in &project.source_limitations {
        limitations.insert(limitation.id.clone(), limitation_from_source(limitation));
    }

    for file in &project.source_files {
        let path = checked_source_path(workspace, file)?;
        let source = fs::read_to_string(&path).map_err(|source| io_error(&path, source))?;
        let mut output = match project.build_adapter {
            BuildAdapter::Vite => instrument_candidate(&source, file),
            BuildAdapter::Generic => instrument_candidate(&source, file),
            BuildAdapter::Direct => instrument_direct_candidate(&source, file),
        }
        .map_err(|source| JavascriptFrontendError::Instrument {
            file: file.clone(),
            source,
        })?;
        if project.build_adapter == BuildAdapter::Generic {
            let runtime = generic_runtime_binding(workspace, project, &path, &generated)?;
            output.code = output.code.replace("virtual:supercov-runtime", &runtime);
            if matches!(
                path.extension().and_then(|value| value.to_str()),
                Some("ts" | "tsx" | "mts" | "cts")
            ) {
                output.code = format!(
                    "// @ts-nocheck -- generated coverage workspace only\n{}",
                    output.code
                );
            }
        }
        if project.build_adapter == BuildAdapter::Vite {
            vite_transforms.insert(
                file.clone(),
                ViteTransform {
                    source_sha256: format!("{:x}", Sha256::digest(source.as_bytes())),
                    code: output.code.clone(),
                    map: output.map.clone(),
                },
            );
        } else {
            atomic_write(&path, output.code.as_bytes())?;
        }
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
    write_vite_transforms(&generated, &vite_transforms)?;

    let mut assertion_calls = 0;
    for entry in &project.source_scope.entries {
        let path = checked_source_path(workspace, &entry.file)?;
        let Ok(source) = fs::read_to_string(&path) else {
            continue;
        };
        let output = instrument_node_assertion_phases_with_expect_modules(
            &source,
            &entry.file,
            std::slice::from_ref(&project.playwright_module),
        )
        .map_err(|source| JavascriptFrontendError::Instrument {
            file: entry.file.clone(),
            source,
        })?;
        let coverage_transformed_by_vite = project.build_adapter == BuildAdapter::Vite
            && project.source_files.contains(&entry.file);
        if output.assertions > 0 && !coverage_transformed_by_vite {
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
        playwright_config_path,
        vite_config_path,
        vitest_config_path,
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
    fn copied_runtime_does_not_reference_unshipped_source_maps() {
        let generated = temporary("runtime-source-maps");
        copy_runtime(None, &generated, "collector-test").unwrap();
        for name in ["vitest.js", "provenance.js", "atomic.js"] {
            let contents = fs::read_to_string(generated.join(name)).unwrap();
            assert!(
                !contents.contains("sourceMappingURL"),
                "runtime shim retained a source-map directive: {name}"
            );
        }
        fs::remove_dir_all(generated).unwrap();
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
            prepare_javascript_frontend(&workspace, &project, Some(&runtime), "collector-test")
                .unwrap();
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
        assert!(prepared.playwright_config_path.is_file());
        assert!(prepared.vite_config_path.is_file());
        assert!(prepared.vitest_config_path.is_file());
        assert_eq!(prepared.assertion_calls, 0);
        fs::remove_dir_all(source_root).unwrap();
        fs::remove_dir_all(workspace).unwrap();
        fs::remove_dir_all(runtime).unwrap();
    }

    #[test]
    fn embedded_runtime_contains_every_declared_shim() {
        for name in RUNTIME_FILES {
            let bytes = embedded_runtime(name).unwrap();
            assert!(!bytes.is_empty(), "embedded runtime is empty: {name}");
        }
        assert!(
            std::str::from_utf8(embedded_runtime("runtime.js").unwrap())
                .unwrap()
                .contains(RUNTIME_INSTANCE_MARKER)
        );
    }
}
