//! A Rust crate compiled to wasm that JavaScript tests load: a library
//! wasm-bindgen exposes to a Node suite (Vitest, Jest, node:test), or the
//! wasm of a web app a Playwright suite drives.
//!
//! The crate is instrumented in the suite's copy, and the suite's own command
//! builds it there (`wasm-pack build`, `cargo build` and `wasm-bindgen`), so
//! the module the tests load carries the probes. Its regions live in the
//! module's memory; the JavaScript runtime remembers each instance and takes
//! the regions at every test's edges, in the test process and, through the
//! Playwright fixture, in each page. Each take belongs to the test that just
//! ended, exactly, and the run publishes them as a Rust run of its own.

use crate::fault_fs as fs;
use std::{
    collections::BTreeMap,
    io::Write,
    path::{Path, PathBuf},
};

use base64::Engine as _;
use serde_json::Value;

use crate::{
    integrity::{FrontendIntegrityInputs, create_explicit_run_integrity},
    lifecycle::{note_kept_evidence, remove_stored_tree_deferred},
    run_store::{RunIntegrity, RunMetadata},
    rust_project::PreparedRustProject,
    rust_wasm_evidence::{Owner, WasmRecord, WasmRun},
    served_tests::ServedTest,
};

/// The crates under `root` that compile to a wasm library: a `cdylib` that
/// depends on `wasm-bindgen`. A Worker's crate, which `worker-build` builds,
/// is measured by `rust_worker` instead.
fn wasm_crates(root: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    wasm_crates_under(root, 0, &mut found);
    found.sort();
    found
}

/// The wasm libraries at most six directories below `directory`, which is
/// `depth` below the root, added to `found`.
fn wasm_crates_under(directory: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    const SKIPPED: &[&str] = &[
        "node_modules",
        "target",
        "build",
        "dist",
        "pkg",
        ".git",
        ".supercov",
        ".wrangler",
    ];
    let Ok(entries) = fs::read_dir(directory) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        let Ok(kind) = fs::file_type(&entry) else {
            continue;
        };
        if kind.is_dir() {
            if depth < 6 && !SKIPPED.contains(&name) {
                wasm_crates_under(&path, depth + 1, found);
            }
            continue;
        }
        if kind.is_file() && name == "Cargo.toml" && is_wasm_library(&path) {
            found.push(path);
        }
    }
}

fn is_wasm_library(manifest: &Path) -> bool {
    let Ok(text) = fs::read_to_string(manifest) else {
        return false;
    };
    let Ok(document) = text.parse::<toml::Table>() else {
        return false;
    };
    let cdylib = document
        .get("lib")
        .and_then(|lib| lib.get("crate-type"))
        .and_then(toml::Value::as_array)
        .is_some_and(|types| types.iter().any(|kind| kind.as_str() == Some("cdylib")));
    let depends = |table: Option<&toml::Value>| {
        table
            .and_then(|dependencies| dependencies.get("wasm-bindgen"))
            .is_some()
    };
    // `[dependencies]`, or `[target.'cfg(..)'.dependencies]`.
    let bindgen = depends(document.get("dependencies"))
        || document
            .get("target")
            .and_then(toml::Value::as_table)
            .is_some_and(|targets| {
                targets
                    .values()
                    .any(|target| depends(target.get("dependencies")))
            });
    let worker = ["wrangler.toml", "wrangler.json", "wrangler.jsonc"]
        .iter()
        .any(|name| manifest.with_file_name(name).is_file());
    cdylib && bindgen && !worker
}

/// The wasm crates of one Cargo workspace JavaScript tests may load.
pub struct RustWasmServer {
    project: PreparedRustProject,
    integrity: RunIntegrity,
    source_inputs: crate::source_manifest::Inputs,
    source_roots: Option<Vec<String>>,
    /// The copy, whose `.supercov/evidence/<run>` holds the tests' records.
    workspace: PathBuf,
    /// Where the JavaScript runtime writes its takes.
    records: PathBuf,
    /// `.supercov/work/<run>/rust-wasm-<n>`.
    directory: PathBuf,
}

impl RustWasmServer {
    /// Whether `root` holds a wasm library JavaScript tests could load.
    pub fn present(root: &Path) -> bool {
        !wasm_crates(root).is_empty()
    }

    /// Instrument the copy's wasm crates, one server per Cargo workspace.
    pub fn prepare(
        root: &Path,
        workspace: &Path,
        server_evidence_root: &Path,
        run_id: &str,
        command: &[String],
    ) -> Result<Vec<Self>, String> {
        let mut by_workspace = BTreeMap::<PathBuf, ()>::new();
        for manifest in wasm_crates(workspace) {
            let crate_directory = manifest.parent().unwrap_or(workspace).to_owned();
            let cargo_root = crate::rust_worker::cargo_workspace_root(&crate_directory)
                .unwrap_or(crate_directory);
            by_workspace.insert(cargo_root, ());
        }
        let mut servers = Vec::new();
        for (index, cargo_root) in by_workspace.into_keys().enumerate() {
            let Ok(relative) = cargo_root.strip_prefix(workspace) else {
                continue;
            };
            let source_roots = crate::source_discovery::configured_source_roots(
                &std::env::vars().collect::<BTreeMap<_, _>>(),
            );
            // The project's inputs, as a later query recomputes them to judge
            // whether the run is current.
            let mut integrity_inputs = crate::rust_run::collect_integrity_inputs(root, command)?;
            crate::source_discovery::fold_roots_into_configuration(
                &mut integrity_inputs.execution_configuration,
                source_roots.as_deref(),
            );
            let source_inputs =
                crate::source_capture::capture(root, "rust", integrity_inputs.assertion_paths())?;
            let integrity = create_explicit_run_integrity(
                root,
                &integrity_inputs,
                &FrontendIntegrityInputs::embedded_rust(),
            )
            .map_err(|error| error.to_string())?;
            let cfg = crate::rust_worker::wasm_target_cfg(&cargo_root);
            let mut project =
                crate::rust_project::prepare_rust_project_for(&cargo_root, None, cfg.as_ref())
                    .map_err(|error| error.to_string())?;
            crate::rust_wasm_evidence::root_paths(&mut project, relative);
            let directory = root
                .join(".supercov/work")
                .join(run_id)
                .join(format!("rust-wasm-{index}"));
            fs::create_dir_all(&directory).map_err(|error| error.to_string())?;
            servers.push(Self {
                project,
                integrity,
                source_inputs,
                source_roots,
                workspace: workspace.to_owned(),
                records: server_evidence_root.join("wasm"),
                directory,
            });
        }
        Ok(servers)
    }

    pub fn source_files(&self) -> usize {
        self.project.source_files.len()
    }

    /// The source this crate was instrumented from.
    pub fn source_fingerprint(&self) -> String {
        self.integrity.fingerprint.source.clone()
    }

    /// The test processes remember wasm instances and take their regions;
    /// the build caps lints at `warn` (Cargo reads wasm32's dropped crate
    /// types from rustc's warnings) and allows the probes' parentheses.
    pub fn environment(&self) -> BTreeMap<String, String> {
        wasm_environment(std::env::var("RUSTFLAGS").unwrap_or_default())
    }

    /// Publish what the tests' wasm ran as a Rust run: each take credited to
    /// the attempt it was taken at the end of. None when no wasm ran.
    pub fn publish(
        self,
        root: &Path,
        evidence_run_id: &str,
        metadata: RunMetadata,
        _served: &[ServedTest],
        diagnostics: &mut dyn Write,
    ) -> Result<Option<(PathBuf, RunMetadata)>, String> {
        // Every runner's attempts: a Node suite reports none of Playwright's.
        let tests = crate::served_tests::read_attempts(
            &self
                .workspace
                .join(".supercov/evidence")
                .join(evidence_run_id),
        );
        let result = read_records(&self.records).and_then(|records| {
            crate::rust_wasm_evidence::publish(
                root,
                WasmRun {
                    language: "Rust wasm",
                    archive: "rust-wasm-evidence.raw.gz",
                    project: &self.project,
                    integrity: &self.integrity,
                    source_inputs: &self.source_inputs,
                    source_roots: self.source_roots.clone(),
                },
                records,
                metadata,
                &tests,
                diagnostics,
            )
        });
        let result = result.map_err(|error| {
            note_kept_evidence(
                root,
                &self.records,
                &format!("{evidence_run_id}-rust-wasm"),
                error,
            )
        });
        let _ = remove_stored_tree_deferred(root, &self.directory);
        result
    }
}

/// `RustWasmServer::environment`, after the user's own `rustflags`.
fn wasm_environment(mut rustflags: String) -> BTreeMap<String, String> {
    if !rustflags.is_empty() {
        rustflags.push(' ');
    }
    rustflags.push_str("--cap-lints=warn -A unused_parens");
    BTreeMap::from([
        ("SUPERCOV_WASM_REGIONS".into(), "1".into()),
        ("RUSTFLAGS".into(), rustflags),
    ])
}

/// The takes the JavaScript runtime wrote, one JSON line each.
fn read_records(directory: &Path) -> Result<Vec<WasmRecord>, String> {
    let mut files = fs::read_dir(directory)
        .map(|entries| {
            entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| {
                    path.extension()
                        .is_some_and(|extension| extension == "jsonl")
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    files.sort();
    let mut records = Vec::new();
    for file in &files {
        let text =
            fs::read_to_string(file).map_err(|error| format!("{}: {error}", file.display()))?;
        // A line cut short by a process killed mid-write is skipped.
        for line in text.lines() {
            let Ok(value) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            let owner = match value.get("owner").and_then(Value::as_str) {
                Some(attempt) => Owner::Attempt(attempt.to_owned()),
                None => Owner::Nobody,
            };
            if let Some(regions) = regions_from(&value) {
                records.push(WasmRecord { owner, regions });
            }
        }
    }
    Ok(records)
}

/// Each region of a take, rebuilt from its header and touched pages.
pub(crate) fn regions_from(value: &Value) -> Option<Vec<(String, Vec<u8>)>> {
    let mut regions = Vec::new();
    for region in value.get("regions")?.as_array()? {
        let crate_key = region.get(0)?.as_str()?.to_owned();
        // A length or offset is far below what a usize holds on any host.
        let length = region.get(1)?.as_u64()? as usize;
        let mut bytes = vec![0_u8; length];
        for segment in region.get(2)?.as_array()? {
            let offset = segment.get(0)?.as_u64()? as usize;
            let data = base64::engine::general_purpose::STANDARD
                .decode(segment.get(1)?.as_str()?)
                .ok()?;
            bytes
                .get_mut(offset..offset.checked_add(data.len())?)?
                .copy_from_slice(&data);
        }
        regions.push((crate_key, bytes));
    }
    Some(regions)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A take's regions are rebuilt from a well-formed record, and any part
    /// of one missing or of the wrong kind is no regions at all.
    #[test]
    fn a_take_s_regions_are_read_or_refused_whole() {
        use serde_json::json;
        assert_eq!(
            regions_from(&json!({"regions": [["abc", 4, [[1, "AQI="]]], ["def", 2, []]]})),
            Some(vec![
                ("abc".into(), vec![0, 1, 2, 0]),
                ("def".into(), vec![0, 0])
            ])
        );
        for refused in [
            json!({}),
            json!({"regions": {}}),
            json!({"regions": [[]]}),
            json!({"regions": [[1]]}),
            json!({"regions": [["abc"]]}),
            json!({"regions": [["abc", "4"]]}),
            json!({"regions": [["abc", 4]]}),
            json!({"regions": [["abc", 4, {}]]}),
            json!({"regions": [["abc", 4, [[]]]]}),
            json!({"regions": [["abc", 4, [["1", "AQI="]]]]}),
            json!({"regions": [["abc", 4, [[1]]]]}),
            json!({"regions": [["abc", 4, [[1, 2]]]]}),
            json!({"regions": [["abc", 4, [[1, "not base64!"]]]]}),
            json!({"regions": [["abc", 4, [[3, "AQI="]]]]}),
            json!({"regions": [["abc", 4, [[u64::MAX, "AQI="]]]]}),
        ] {
            assert_eq!(regions_from(&refused), None, "{refused}");
        }
    }

    #[test]
    fn a_wasm_library_is_a_cdylib_that_depends_on_wasm_bindgen() {
        let root = std::env::temp_dir().join(format!(
            "supercov-rust-wasm-crates-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let write = |relative: &str, text: &str| {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        };
        write(
            "rust/Cargo.toml",
            "[package]\nname = \"a\"\n[lib]\ncrate-type = [\"cdylib\", \"rlib\"]\n[dependencies]\nwasm-bindgen = \"0.2\"\n",
        );
        // A native crate, a cdylib that is no wasm library, a Worker, and a
        // crate in a dependency are not.
        write(
            "native/Cargo.toml",
            "[package]\nname = \"b\"\n[dependencies]\nwasm-bindgen = \"0.2\"\n",
        );
        write(
            "plugin/Cargo.toml",
            "[package]\nname = \"c\"\n[lib]\ncrate-type = [\"cdylib\"]\n",
        );
        write(
            "worker/Cargo.toml",
            "[package]\nname = \"d\"\n[lib]\ncrate-type = [\"cdylib\"]\n[dependencies]\nwasm-bindgen = \"0.2\"\n",
        );
        write("worker/wrangler.toml", "main = \"x\"\n");
        // A wasm-bindgen dependency for the wasm target alone counts.
        write(
            "targeted/Cargo.toml",
            "[package]\nname = \"f\"\n[lib]\ncrate-type = [\"cdylib\"]\n[target.'cfg(target_arch = \"wasm32\")'.dependencies]\nwasm-bindgen = \"0.2\"\n",
        );
        write(
            "node_modules/e/Cargo.toml",
            "[package]\nname = \"e\"\n[lib]\ncrate-type = [\"cdylib\"]\n[dependencies]\nwasm-bindgen = \"0.2\"\n",
        );
        let found = wasm_crates(&root)
            .into_iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            found,
            [
                PathBuf::from("rust/Cargo.toml"),
                PathBuf::from("targeted/Cargo.toml")
            ]
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn the_build_caps_lints_after_the_user_s_own_flags() {
        assert_eq!(
            wasm_environment(String::new())["RUSTFLAGS"],
            "--cap-lints=warn -A unused_parens"
        );
        assert_eq!(
            wasm_environment("--cfg x".into())["RUSTFLAGS"],
            "--cfg x --cap-lints=warn -A unused_parens"
        );
    }

    /// A line cut short and a take with no regions are passed over; a take
    /// at a test's edge is that attempt's, and any other nobody's.
    #[test]
    fn records_are_read_from_whole_takes_only() {
        let directory = std::env::temp_dir().join(format!(
            "supercov-rust-wasm-records-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&directory).unwrap();
        fs::write(
            directory.join("a.jsonl"),
            "{\"owner\": \"t1\", \"regions\": []}\n{\"owner\": \"t2\"}\n{\"regions\": [\n",
        )
        .unwrap();
        fs::write(directory.join("b.jsonl"), "{\"regions\": []}\n").unwrap();
        fs::write(directory.join("notes.txt"), "{\"regions\": []}\n").unwrap();
        fs::write(directory.join("c.jsonl"), "").unwrap();
        assert!(read_records(&directory.join("absent")).unwrap().is_empty());
        // No wasm library in the workspace is no server to prepare.
        assert!(
            RustWasmServer::prepare(&directory, &directory, &directory, "run", &[])
                .unwrap()
                .is_empty()
        );
        let owners = read_records(&directory)
            .unwrap()
            .into_iter()
            .map(|record| record.owner)
            .collect::<Vec<_>>();
        assert_eq!(owners, [Owner::Attempt("t1".into()), Owner::Nobody]);
        fs::remove_dir_all(directory).unwrap();
    }
}
