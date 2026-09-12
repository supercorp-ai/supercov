//! Internal calibration reader, not a public assertion-score command.
//! cargo run -p supercov-engine --example rust_asserted_runtime -- evidence.raw.gz src/lib.rs
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    time::Instant,
};

use serde_json::{Value, json};
use supercov_engine::{
    asserted_coverage::join,
    coverage_report::{CoverageManifest, RawTestResult},
    evidence_archive::read_archive,
    run_store::RunMetadata,
    rust_asserted_coverage::analyze_runtime_assertions,
    rust_asserted_source::{RustAssertionSources, analyze_source_assertions},
    rust_project::discover_rust_source_files,
    rust_run::current_rust_integrity,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args: Vec<_> = std::env::args().skip(1).collect();
    let source_root = if let Some(index) = args.iter().position(|a| a == "--source-root") {
        if index + 2 != args.len() {
            return Err("--source-root PATH must be the last argument pair".into());
        }
        let root = args.pop().unwrap();
        args.pop();
        Some(root)
    } else {
        None
    };
    if args.len() < 2 {
        return Err("expected archive and one or more explicitly scoped source files".into());
    }
    let started = Instant::now();
    let entries = read_archive(Path::new(&args[0]))?;
    let archive_read_ms = started.elapsed().as_secs_f64() * 1000.0;
    let manifest: CoverageManifest = serde_json::from_slice(
        &entries
            .iter()
            .find(|e| e.path == "manifest.json")
            .ok_or("missing manifest")?
            .contents,
    )?;
    let health: Vec<Value> = serde_json::from_slice(
        &entries
            .iter()
            .find(|e| e.path == "rust/transport-health.json")
            .ok_or("missing Rust transport health")?
            .contents,
    )?;
    if health.is_empty()
        || health.iter().any(|h| {
            h["transport"]["incomplete"].as_u64() != Some(0)
                || h["transport"]["dropped"].as_u64() != Some(0)
        })
    {
        return Err("cannot calibrate incomplete/dropped Rust evidence".into());
    }
    let results: Vec<RawTestResult> = entries
        .iter()
        .filter(|e| e.path.starts_with("results/") && e.path.ends_with("/mcdc.json"))
        .map(|e| serde_json::from_slice(&e.contents))
        .collect::<Result<_, _>>()?;
    if results.is_empty() {
        return Err("archive has no test results".into());
    }
    let files: BTreeSet<_> = args[1..].iter().cloned().collect();
    let loaded_ms = started.elapsed().as_secs_f64() * 1000.0;
    let analysis = analyze_runtime_assertions(&manifest, &results, &files)?;
    if analysis.facts.sites.is_empty() || !analysis.unmeasured.is_empty() {
        return Err("calibration requires a nonempty, fully measured function inventory; do not drop unmeasured functions".into());
    }
    let resolutions = join(&analysis.facts);
    let analysis_ms = started.elapsed().as_secs_f64() * 1000.0 - loaded_ms;
    let source_started = Instant::now();
    let mut integrity_ms = 0.0;
    let mut source_read_ms = 0.0;
    let mut source_analyze_ms = 0.0;
    let source_analysis = if let Some(root) = &source_root {
        let root = Path::new(root);
        let metadata: RunMetadata = serde_json::from_slice(&std::fs::read(
            Path::new(&args[0])
                .parent()
                .ok_or("missing run directory")?
                .join("run.json"),
        )?)?;
        let current = current_rust_integrity(root, &metadata.command)?;
        let expected = &metadata.integrity.fingerprint;
        if current.fingerprint.source != expected.source
            || current.fingerprint.tests != expected.tests
            || current.fingerprint.dependencies != expected.dependencies
            || current.fingerprint.configuration != expected.configuration
        {
            return Err("source/dependencies/configuration changed since this run".into());
        }
        integrity_ms = source_started.elapsed().as_secs_f64() * 1000.0;
        let read_started = Instant::now();
        let cargo: toml::Value =
            toml::from_str(&std::fs::read_to_string(root.join("Cargo.toml"))?)?;
        // This reader intentionally supports a single dependency-free Cargo
        // library. No guessed workspace/renamed-dependency resolution.
        if cargo.get("dependencies").is_some()
            || cargo.get("dev-dependencies").is_some()
            || cargo.get("build-dependencies").is_some()
            || cargo.get("target").is_some()
            || cargo
                .get("workspace")
                .and_then(|w| w.get("members"))
                .is_some()
            || cargo.get("package").and_then(|p| p.get("build")).is_some()
            || root.join("build.rs").exists()
        {
            return Err("source calibration currently requires a single library without dependency aliases or build scripts".into());
        }
        let name = cargo
            .get("lib")
            .and_then(|v| v.get("name"))
            .and_then(toml::Value::as_str)
            .or_else(|| {
                cargo
                    .get("package")
                    .and_then(|v| v.get("name"))
                    .and_then(toml::Value::as_str)
            })
            .ok_or("missing Cargo library name")?
            .replace('-', "_");
        if matches!(name.as_str(), "std" | "core" | "alloc") {
            return Err("reserved library identity".into());
        }
        let library_file = cargo
            .get("lib")
            .and_then(|v| v.get("path"))
            .and_then(toml::Value::as_str)
            .unwrap_or("src/lib.rs")
            .to_owned();
        let snapshots = discover_rust_source_files(root)?
            .into_iter()
            .map(|file| std::fs::read_to_string(root.join(&file)).map(|source| (file, source)))
            .collect::<Result<BTreeMap<_, _>, _>>()?;
        source_read_ms = read_started.elapsed().as_secs_f64() * 1000.0;
        let analyze_started = Instant::now();
        let analyzed = analyze_source_assertions(
            &manifest,
            &results,
            &files,
            &RustAssertionSources {
                files: snapshots,
                crate_name: name,
                library_file,
            },
            expected,
        )?;
        source_analyze_ms = analyze_started.elapsed().as_secs_f64() * 1000.0;
        Some(analyzed)
    } else {
        None
    };
    let source_ms = source_started.elapsed().as_secs_f64() * 1000.0;
    let join_started = Instant::now();
    let source_resolutions = source_analysis.as_ref().map(|a| join(&a.facts));
    let source_join_ms = join_started.elapsed().as_secs_f64() * 1000.0;
    let points: Vec<_> = manifest
        .points
        .iter()
        .filter(|p| analysis.facts.sites.iter().any(|s| s.id == p.id))
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "analysis": analysis, "resolutions": resolutions, "points": points,
            "loadMs": loaded_ms, "analysisAndJoinMs": analysis_ms,
            "health": health, "testResults": results.len(),
            "manifestLimitations": manifest.limitations,
            "coverageInventory": {"points":manifest.points,"branches":manifest.branches,
                "decisions":manifest.decisions,"unmeasured":manifest.unmeasured},
            "compilerAssertionIdentities": manifest.scope.as_ref().and_then(|s| s.get("assertionIdentities")),
            "sourceAnalysis": source_analysis, "sourceResolutions": source_resolutions,
            "sourceReadVerifyAnalyzeMs": source_ms,
            "queryTimings": {"archiveReadMs":archive_read_ms,
                "evidenceDecodeMs":loaded_ms - archive_read_ms,
                "runtimeAnalyzeAndJoinMs":analysis_ms, "integrityMs":integrity_ms,
                "sourceReadMs":source_read_ms,"sourceAnalyzeMs":source_analyze_ms,
                "sourceJoinMs":source_join_ms}
        }))?
    );
    Ok(())
}
