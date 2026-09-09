//! Read-only JS/TS query. No assertion inference runs during tests.
#[path = "asserted_paging.rs"]
mod paging;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Command, ExitCode, Stdio},
};
use supercov_engine::{
    asserted_coverage::{Facts, HintValidation, PragmaHint, check_pragma_hints, join},
    coverage_report::{CoverageManifest, RawTestResult, ServerRecord},
    evidence_archive::read_archive,
    javascript_run::current_javascript_integrity,
    js_sites::discover_effect_sites,
    project_discovery::discover_coverage_project,
    run_store::{StoredRun, discover_runs, select_run},
};

const HELP: &str = "Usage: supercov runs <run-id> assertions [--pragmas | --evidence <pointer>] [--file <path>] [--site <id>] [--offset <n>] [--limit <n>] [--analysis <sha256>] [--json]\n\nAnalyze which source behaviors are linked to test assertions, with evidence, gaps and analysis limits.\nJS/TS candidate evidence, not a proven assertion score.\nRuns after tests using the existing archive, statement markers and assertion phases.\nRequires a matching project, its TypeScript compiler API, and the packaged first-party analyzer.\n\n--pragmas  Page through user-suggested assertion links and their validation.\n           Leading assertion comments: // observes: <file>#<function> [snippet]\n           Targets must resolve to one site; comments never add assertion credit.\n           --file/--site filter the suggested production targets.\n--evidence Page a returned JSON pointer (for example /tests or /sites/0/facts).\n           Objects/arrays return items; string leaves return Unicode-scalar text chunks.\n           Cannot combine with --file, --site or --pragmas.\n--analysis Require the analysisId returned by the first page; prevents mixing analyses.\n\nPages adapt to the response budget. Follow pagination.nextOffset, not offset + limit.\nLarge records carry detailOnly and an evidence.pointer; all details remain readable.\n";
pub const AGENT_COMMAND: &str = "coverage.assertions";

fn protocol() -> Value {
    json!({"abi":1,"factsSchema":1,"rules":"source-linked-v3/archive-3","capabilities":["requiresTotal-v1", "assertion-witness-issues-v1", "assertion-hints-v1", "mock-observation-projections-v1", "assertion-comparison-relations-v1", "mock-count-lifetimes-v1", "mock-count-factories-v1", "mock-count-rows-v1"]})
}
#[derive(Default)]
struct Options {
    file: Option<String>,
    site: Option<String>,
    offset: usize,
    limit: usize,
    pragmas: bool,
    evidence: Option<String>,
    analysis: Option<String>,
}
fn parse(args: &[String]) -> Result<Options, String> {
    let mut options = Options {
        limit: 20,
        ..Default::default()
    };
    let mut seen = BTreeSet::new();
    let mut args = args.iter();
    while let Some(arg) = args.next() {
        if !seen.insert(arg) {
            return Err(format!("Duplicate option: {arg}"));
        }
        if arg == "--json" {
            continue;
        }
        if arg == "--pragmas" {
            options.pragmas = true;
            continue;
        }
        if !matches!(
            arg.as_str(),
            "--file" | "--site" | "--offset" | "--limit" | "--evidence" | "--analysis"
        ) {
            return Err(format!("Unknown assertion-query option: {arg}"));
        }
        let value = args
            .next()
            .filter(|v| !v.starts_with("--"))
            .ok_or_else(|| format!("{arg} requires a value"))?;
        match arg.as_str() {
            "--file" => options.file = Some(value.clone()),
            "--site" => options.site = Some(value.clone()),
            "--evidence" => options.evidence = Some(value.clone()),
            "--analysis" => {
                if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err("--analysis requires a SHA-256 analysisId".into());
                }
                options.analysis = Some(value.to_lowercase());
            }
            "--offset" => options.offset = value.parse().map_err(|_| "Invalid offset")?,
            "--limit" => {
                options.limit = value.parse().map_err(|_| "Invalid limit")?;
                if options.limit == 0 {
                    return Err("limit must be positive".into());
                }
            }
            _ => unreachable!(),
        }
    }
    if options.evidence.is_some()
        && (options.pragmas || options.file.is_some() || options.site.is_some())
    {
        return Err("--evidence cannot combine with --pragmas, --file or --site".into());
    }
    Ok(options)
}
fn fresh(root: &Path, run: &StoredRun) -> Result<(), String> {
    let now = current_javascript_integrity(root, &run.metadata.command)?;
    let old = &run.metadata.integrity.fingerprint;
    let new = &now.fingerprint;
    let mut reasons = Vec::new();
    for (name, a, b) in [
        ("source", &old.source, &new.source),
        ("tests", &old.tests, &new.tests),
        ("dependencies", &old.dependencies, &new.dependencies),
        ("configuration", &old.configuration, &new.configuration),
        ("instrumenter", &old.instrumenter, &new.instrumenter),
    ] {
        if a != b {
            reasons.push(name);
        }
    }
    if reasons.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "Assertion analysis refused stale run: {} changed; rerun the tests",
            reasons.join(", ")
        ))
    }
}
fn local_file(root: &Path, file: &str) -> Result<PathBuf, String> {
    let path = Path::new(file);
    if path
        .components()
        .any(|p| !matches!(p, Component::Normal(_)))
    {
        return Err(format!("Non-local source path: {file}"));
    }
    let canonical = root.join(path).canonicalize().map_err(|e| e.to_string())?;
    if !canonical.starts_with(root) {
        return Err(format!("Source outside project: {file}"));
    }
    Ok(canonical)
}
fn query(root: &Path, selector: &str, options: Options) -> Result<Value, String> {
    let root = root.canonicalize().map_err(|e| e.to_string())?;
    let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
    let run = select_run(&inventory, Some(selector)).map_err(|e| e.to_string())?;
    if run.metadata.merged == Some(true) {
        return Err("Merged runs need a parent-aware assertion adapter; query a single run".into());
    }
    fresh(&root, run)?;
    let archive_digest = || {
        fs::read(&run.evidence_path)
            .map(|b| format!("{:x}", Sha256::digest(b)))
            .map_err(|e| e.to_string())
    };
    let evidence_sha256 = archive_digest()?;
    let entries = read_archive(&run.evidence_path).map_err(|e| e.to_string())?;
    let entry = |name: &str| {
        entries
            .iter()
            .find(|e| e.path == name)
            .ok_or_else(|| format!("Missing {name}"))
    };
    let frontend: Value =
        serde_json::from_slice(&entry("frontend.json")?.contents).map_err(|e| e.to_string())?;
    if frontend["language"] != "javascript" {
        return Err("This assertion query supports JS/TS archives only".into());
    }
    let manifest: CoverageManifest =
        serde_json::from_slice(&entry("manifest.json")?.contents).map_err(|e| e.to_string())?;
    let project = discover_coverage_project(
        &root,
        &std::env::vars().collect::<BTreeMap<_, _>>(),
        &run.metadata.command,
    )
    .map_err(|e| e.to_string())?;
    let mut effects = Vec::new();
    let mut limitations = BTreeSet::new();
    for file in &project.source_files {
        let source = fs::read_to_string(local_file(&root, file)?).map_err(|e| e.to_string())?;
        let discovered = discover_effect_sites(file, &source).map_err(|e| e.to_string())?;
        effects.extend(discovered.sites);
        limitations.extend(discovered.limitations);
    }
    let mut records = Vec::<RawTestResult>::new();
    let mut server_records = Vec::<ServerRecord>::new();
    for entry in &entries {
        if entry.path.contains("-status") {
            continue;
        }
        if entry.path.ends_with("/mcdc.json") {
            records.push(
                serde_json::from_slice(&entry.contents)
                    .map_err(|e| format!("{}: {e}", entry.path))?,
            );
        } else if entry.path.ends_with(".mcdc.jsonl") {
            for line in entry
                .contents
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
            {
                records.push(
                    serde_json::from_slice(line).map_err(|e| format!("{}: {e}", entry.path))?,
                );
            }
        } else if entry.path.starts_with("server/") && entry.path.ends_with(".jsonl") {
            if entry.path.starts_with("server/background/") {
                limitations.insert(
                    "Background/unattributed server evidence cannot lend assertion witnesses"
                        .to_owned(),
                );
                continue;
            }
            for line in entry
                .contents
                .split(|b| *b == b'\n')
                .filter(|l| !l.is_empty())
            {
                server_records.push(
                    serde_json::from_slice(line).map_err(|e| format!("{}: {e}", entry.path))?,
                );
            }
        }
    }
    for record in server_records {
        let Some(scope) = record.scope.as_ref() else {
            limitations.insert("Server records without attempt scopes remain unjoined".to_owned());
            continue;
        };
        let matches = records
            .iter()
            .enumerate()
            .filter(|(_, raw)| raw.scope.as_ref() == Some(scope))
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if let [index] = matches.as_slice() {
            if !records[*index].server.contains(&record) {
                records[*index].server.push(record);
            }
        } else {
            limitations.insert(
                "Server records without one exact matching attempt remain unjoined".to_owned(),
            );
        }
    }
    for record in &records {
        if let Some(file) = &record.test_file {
            local_file(&root, file)?;
        }
    }
    if records.is_empty() {
        return Err("No supported per-test records in archive".into());
    }
    let input = json!({ "protocol":protocol(), "projectRoot":root, "runId":run.id, "sourceFiles":project.source_files,
        "effects":effects, "manifest":manifest, "records":records, "limitations":limitations });
    let package = std::env::var_os("SUPERCOV_PACKAGE_ROOT").map(PathBuf::from)
        .or_else(|| cfg!(debug_assertions).then(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")))
        .ok_or("JS/TS assertion queries require the analyzer shipped with the npm package. Invoke the npm Supercov launcher, or set SUPERCOV_PACKAGE_ROOT to its installed package directory.")?;
    let script = package.join("analyzers/typescript/bin/query.mjs");
    if !script.is_file() {
        return Err("First-party assertion analyzer is unavailable in this distribution".into());
    }
    let mut child = Command::new("node")
        .arg(&script)
        .current_dir(&root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start assertion analyzer: {e}"))?;
    let written = child
        .stdin
        .take()
        .expect("piped input")
        .write_all(&serde_json::to_vec(&input).map_err(|e| e.to_string())?);
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "Assertion analyzer failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    written.map_err(|e| e.to_string())?;
    let result: Value = serde_json::from_slice(&output.stdout).map_err(|e| e.to_string())?;
    if result["protocol"] != protocol() {
        return Err("Unsupported analyzer version, facts schema or capabilities".into());
    }
    if result["analyzer"]["schema"] != 1 {
        return Err("Unsupported analyzer build identity".into());
    }
    for field in ["sourceSha256", "compiledSha256", "compilerSha256"] {
        if !result["analyzer"][field]
            .as_str()
            .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(format!("Missing analyzer identity: {field}"));
        }
    }
    fresh(&root, run)?;
    if archive_digest()? != evidence_sha256 {
        return Err("Archive changed during assertion query".into());
    }
    let facts: Facts =
        serde_json::from_value(result["facts"].clone()).map_err(|e| e.to_string())?;
    if facts.schema != 1 {
        return Err("Unsupported assertion facts schema".into());
    }
    let sites = result["inventory"]
        .as_array()
        .ok_or("Missing site inventory")?;
    let ids = sites
        .iter()
        .map(|s| s["id"].as_str().ok_or("Missing site id"))
        .collect::<Result<BTreeSet<_>, _>>()?;
    if ids.len() != sites.len()
        || facts.sites.len() != sites.len()
        || facts
            .sites
            .iter()
            .map(|s| s.id.as_str())
            .collect::<BTreeSet<_>>()
            != ids
    {
        return Err("Analyzer changed or duplicated the site denominator".into());
    }
    let hints: Vec<PragmaHint> = serde_json::from_value(result["pragmas"].clone())
        .map_err(|e| format!("Invalid assertion hint facts: {e}"))?;
    let checks = check_pragma_hints(&facts, &hints);
    let resolutions = join(&facts);
    let candidate_summary = supercov_engine::asserted_coverage::summary(&facts.sites, &resolutions);
    let by_id = resolutions
        .iter()
        .map(|r| (r.site.as_str(), r))
        .collect::<BTreeMap<_, _>>();
    let facts_by_id = facts
        .sites
        .iter()
        .map(|s| (s.id.as_str(), s))
        .collect::<BTreeMap<_, _>>();
    let rows = sites.iter().map(|s| {
        let id = s["id"].as_str().unwrap();
        json!({"site":s,"analysisCertainty":"unverified","candidate":by_id[id],"facts":facts_by_id[id]})
    }).collect::<Vec<_>>();
    let document = json!({"sites":rows,"pragmas":checks,"tests":result["facts"]["tests"],
        "attempts":result["attempts"],"executionLinks":result["executionLinks"],
        "diagnostics":result["diagnostics"],"limitations":result["limitations"],
        "sourceScope":project.source_scope,"measurementLimitations":manifest.limitations,
        "unmeasured":manifest.unmeasured,"mocksByTestFile":facts.mocks_by_test_file});
    let analysis_id = format!("{:x}", Sha256::digest(serde_json::to_vec(&json!({
        "reportSchema":2,"document":document,"analyzer":result["analyzer"],"protocol":protocol(),
        "evidenceSha256":evidence_sha256,"runFingerprint":run.metadata.integrity.fingerprint
    })).map_err(|e| e.to_string())?));
    if options
        .analysis
        .as_ref()
        .is_some_and(|expected| expected != &analysis_id)
    {
        return Err(
            "Assertion analysis changed; restart pagination with the new analysisId".into(),
        );
    }
    let references = document
        .as_object()
        .unwrap()
        .iter()
        .map(|(key, value)| (key.clone(), paging::reference(value, &format!("/{key}"))))
        .collect::<serde_json::Map<_, _>>();
    let mut base = json!({"runId":run.id,"view":"sites","reportSchema":2,
        "protocol":protocol(),"analyzer":result["analyzer"],"analysisId":analysis_id,
        "evidenceSha256":evidence_sha256,"runFingerprint":run.metadata.integrity.fingerprint,
        "sourceFreshness":"matching-run-fingerprints-before-and-after-query","assertionScore":null,
        "summary":{"sites":sites.len(),"semanticallyVerifiedSites":0,"unverifiedSites":sites.len(),
            "pragmaHints":hints.len(),"contractualCandidates":candidate_summary},"evidence":references});
    if let Some(pointer) = &options.evidence {
        return paging::evidence(base, &document, pointer, options.offset, options.limit);
    }
    if options.pragmas {
        base["view"] = json!("pragmas");
        base["summary"] = json!({"hints":checks.len(),
            "analyzerSupported":checks.iter().filter(|c| c.validation == HintValidation::AnalyzerSupported).count(),
            "unresolved":checks.iter().filter(|c| c.validation == HintValidation::Unresolved).count(),
            "invalid":checks.iter().filter(|c| c.validation == HintValidation::Invalid).count()});
        let selected =
            checks
                .iter()
                .enumerate()
                .filter(|(_, check)| {
                    options.file.as_ref().is_none_or(|file| {
                        check.hint.target.as_ref().is_some_and(|t| &t.file == file)
                    }) && options
                        .site
                        .as_ref()
                        .is_none_or(|id| check.hint.candidate_sites.contains(id))
                })
                .map(|(i, _)| paging::record(&document["pragmas"][i], &format!("/pragmas/{i}")))
                .collect::<Vec<_>>();
        return paging::page(base, "pragmas", &selected, options.offset, options.limit);
    }
    let selected = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            options
                .file
                .as_ref()
                .is_none_or(|f| row["site"]["file"] == *f)
                && options
                    .site
                    .as_ref()
                    .is_none_or(|id| row["site"]["id"] == *id)
        })
        .map(|(i, row)| paging::record(row, &format!("/sites/{i}")))
        .collect::<Vec<_>>();
    paging::page(base, "sites", &selected, options.offset, options.limit)
}
pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let json_output = args.iter().any(|a| a == "--json");
    let result = (|| {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        query(&root, &args[0], parse(&args[2..])?)
    })();
    match result {
        Ok(data) => {
            if json_output {
                match supercov_engine::agent_json::success(AGENT_COMMAND, &data, None) {
                    Ok(json) => print!("{json}"),
                    Err(size) => {
                        print!("{}", supercov_engine::agent_json::failure(Some(AGENT_COMMAND), &supercov_engine::agent_json::AgentError {
                            code: supercov_engine::agent_json::ErrorCode::ResponseTooLarge,
                            message: "Assertion reference metadata exceeds the query budget; no evidence was discarded.".into(),
                            retryable: false, details: Some(json!({"actualBytes":size.actual_bytes,"maxBytes":size.max_bytes})),
                        }));
                        return ExitCode::from(2);
                    }
                }
            } else if data["view"] == "evidence" {
                println!(
                    "Evidence {} for {} (use --json for structured values)",
                    data["path"], data["runId"]
                );
                if let Some(text) = data["text"].as_str() {
                    println!("{text}");
                } else {
                    for item in data["items"].as_array().unwrap() {
                        println!(
                            "{}: {}",
                            item["pointer"],
                            item.get("value").unwrap_or(&item["kind"])
                        );
                    }
                }
            } else if data["view"] == "pragmas" {
                println!(
                    "Assertion hints for {} (user-suggested; not formal proofs)",
                    data["runId"].as_str().unwrap()
                );
                for row in data["pragmas"].as_array().unwrap() {
                    if row["detailOnly"] == true {
                        println!("Large hint: use --evidence {}", row["evidence"]["pointer"]);
                        continue;
                    }
                    println!(
                        "{} — {}; {}{}",
                        row["hint"]["where"].as_str().unwrap(),
                        row["validation"].as_str().unwrap(),
                        row["reason"].as_str().unwrap(),
                        row["strength"]
                            .as_str()
                            .map(|s| format!("; strength {s}"))
                            .unwrap_or_default()
                    );
                }
                println!(
                    "Hints never add assertion credit. Use --json for exact targets and assertion identities."
                );
            } else {
                println!(
                    "Assertion candidates for {} (not a proven assertion score)",
                    data["runId"].as_str().unwrap()
                );
                for row in data["sites"].as_array().unwrap() {
                    if row["detailOnly"] == true {
                        println!(
                            "Large site record: use --evidence {}",
                            row["evidence"]["pointer"]
                        );
                        continue;
                    }
                    println!(
                        "{}:{} {} — candidate {}; unverified",
                        row["site"]["file"].as_str().unwrap(),
                        row["site"]["start"]["line"],
                        row["site"]["category"].as_str().unwrap(),
                        row["candidate"]["status"].as_str().unwrap()
                    );
                }
                println!("Use --json for evidence, limitations, freshness and analyzer identity.");
                if data["summary"]["pragmaHints"].as_u64().unwrap_or(0) > 0 {
                    println!("Use --pragmas to inspect user-suggested links and their validation.");
                }
            }
            if !json_output && data["pagination"]["hasMore"] == true {
                println!(
                    "More results: continue with --offset {} --analysis {}",
                    data["pagination"]["nextOffset"], data["analysisId"]
                );
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            if json_output {
                println!(
                    "{}",
                    supercov_engine::agent_json::failure(
                        Some(AGENT_COMMAND),
                        &supercov_engine::agent_json::AgentError {
                            code: supercov_engine::agent_json::ErrorCode::InvalidArgument,
                            message,
                            retryable: false,
                            details: None
                        }
                    )
                );
            } else {
                eprintln!("[supercov] {message}");
            }
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_options_without_silently_ignoring_filters() {
        for args in [
            vec!["--filter", "failed"],
            vec!["--limit", "0"],
            vec!["--site"],
            vec!["--file", "a", "--file", "b"],
            vec!["--evidence", "/tests", "--site", "x"],
            vec!["--evidence", "/tests", "--pragmas"],
            vec!["--analysis", "bad"],
        ] {
            assert!(parse(&args.into_iter().map(String::from).collect::<Vec<_>>()).is_err());
        }
        assert_eq!(parse(&["--limit".into(), "5".into()]).unwrap().limit, 5);
    }
}
