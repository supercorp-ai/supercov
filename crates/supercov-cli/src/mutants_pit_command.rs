//! `supercov runs <run-id> mutants pit <mutations.xml>`: the run's Java and
//! Kotlin mutants beside PIT's, joined by file, line, mutator and what PIT's
//! description says, with each side's verdict. A mutant PIT detects and
//! Supercov calls survived (or no coverage), or the reverse, is a soundness
//! violation, and the command exits 1. This is how the JVM strong pass is
//! validated, and how a team that runs PIT reads Supercov's reasons beside
//! it.

use std::collections::BTreeMap;
use std::process::ExitCode;

use serde_json::{Value, json};
use supercov_engine::run_store::{discover_runs, select_run};

pub(crate) const HELP: &str = "\
Usage: supercov runs <run-id> mutants pit <mutations.xml>... [--file <path>]... [--json]

Joins the run's Java and Kotlin mutants with PIT's report (outputFormats
XML; fullMutationMatrix lists every killing test) and compares the verdicts:
the strong pass's where it ran, else what the run decided without running.
A mutant PIT detects and Supercov calls survived or no-coverage, or the
reverse, is a soundness violation: the command exits 1.

Options:
  --file <path>  only this source file's mutants (repeatable): those PIT
                 was given, where its run named target classes
  --json         the answer as JSON
";

/// `args` is `[<run-id>, "mutants", "pit", ...]`.
pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let as_json = args.iter().any(|a| a == "--json");
    let mut reports = Vec::new();
    let mut files = Vec::new();
    let mut rest = args[3..].iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--json" => {}
            "--file" => match rest.next() {
                Some(file) => files.push(file.clone()),
                None => {
                    return crate::refuse(
                        "runs.mutants.pit",
                        as_json,
                        "--file needs a path".into(),
                        HELP,
                    );
                }
            },
            other if other.starts_with("--") => {
                return crate::refuse(
                    "runs.mutants.pit",
                    as_json,
                    format!("Unknown option: {other}"),
                    HELP,
                );
            }
            report => reports.push(report.to_owned()),
        }
    }
    if reports.is_empty() {
        return crate::refuse(
            "runs.mutants.pit",
            as_json,
            "give PIT's mutations.xml".into(),
            HELP,
        );
    }
    match compare(&args[0], &reports, &files) {
        Ok(data) => {
            if as_json {
                println!("{}", serde_json::to_string(&data).expect("JSON"));
            } else {
                print!("{}", text(&data));
            }
            if data["soundnessViolations"]["total"].as_u64().unwrap_or(0) > 0 {
                ExitCode::from(1)
            } else {
                ExitCode::SUCCESS
            }
        }
        Err(message) => crate::refuse("runs.mutants.pit", as_json, message, HELP),
    }
}

fn compare(run_id: &str, reports: &[String], only: &[String]) -> Result<Value, String> {
    let root = std::env::current_dir().map_err(|e| e.to_string())?;
    let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
    let run = select_run(&inventory, Some(run_id)).map_err(|e| e.to_string())?;
    let mut pit = Vec::new();
    for report in reports {
        let text = std::fs::read_to_string(report).map_err(|e| format!("{report}: {e}"))?;
        pit.extend(supercov_engine::jvm_strong::parse_pit_report(&text));
    }
    if !only.is_empty() {
        pit.retain(|mutant| only.iter().any(|file| file.ends_with(&mutant.file)));
    }
    let plan = crate::mutants_test_command::plan_of(&root, run)?;
    let saved = crate::mutants_test_command::read_saved(run);
    // Each file the run listed, enumerated again for the join's keys.
    let files = plan
        .mutants
        .iter()
        .map(|mutant| mutant.file.clone())
        .filter(|file| only.is_empty() || only.contains(file))
        .collect::<std::collections::BTreeSet<_>>();
    let sources = supercov_engine::jvm_project::discover_jvm_files(&root)?
        .sources
        .into_iter()
        .map(|(file, _)| file)
        .collect::<Vec<_>>();
    let index = supercov_engine::jvm_strong::index_of(&root, &sources);
    let mut ours = BTreeMap::new();
    let mut texts = BTreeMap::new();
    for file in &files {
        let Ok(source) = std::fs::read_to_string(root.join(file)) else {
            continue;
        };
        if let Ok(listed) = supercov_engine::jvm_strong::file_mutants(file, &source, &index) {
            ours.insert(file.clone(), listed.mutants);
        }
        texts.insert(file.clone(), source);
    }
    let join = supercov_engine::jvm_strong::join_pit(&ours, &pit);
    let status_of = |id: &str| -> String {
        if let Some(verdict) = saved.get(id) {
            return verdict.status.clone();
        }
        let planned = plan.mutants.iter().find(|mutant| mutant.id == id);
        match planned.and_then(|mutant| mutant.decided) {
            Some(supercov_engine::strong_plan::Decided::NoCoverage) => "no-coverage".into(),
            Some(supercov_engine::strong_plan::Decided::Survived) => "survived".into(),
            Some(supercov_engine::strong_plan::Decided::Equivalent) => "equivalent".into(),
            None => "not-run".into(),
        }
    };
    let mut violations = Vec::new();
    let mut agree = 0usize;
    let mut not_compared = BTreeMap::<String, usize>::new();
    for (id, at) in &join.pairs {
        let pit_mutant = &pit[*at];
        let ours = status_of(id);
        let detected = match ours.as_str() {
            "killed" | "timeout" => Some(true),
            "survived" | "no-coverage" | "equivalent" => Some(false),
            _ => None,
        };
        let pit_decided = match pit_mutant.status.as_str() {
            "NON_VIABLE" | "NOT_STARTED" | "STARTED" => None,
            _ => Some(pit_mutant.detected()),
        };
        match (detected, pit_decided) {
            (Some(ours_detected), Some(pit_detected)) if ours_detected == pit_detected => {
                agree += 1;
            }
            (Some(_), Some(_)) => violations.push(json!({
                "id": id,
                "status": ours,
                "pit": pit_mutant.status,
                "pitKillingTests": pit_mutant.killing_tests,
                "killedBy": saved.get(id).and_then(|verdict| verdict.killed_by.clone()),
                "reason": saved.get(id).and_then(|verdict| verdict.reason.clone()),
            })),
            _ => {
                *not_compared
                    .entry(format!("{ours} / {}", pit_mutant.status))
                    .or_default() += 1;
            }
        }
    }
    let mut only_pit = BTreeMap::<String, BTreeMap<String, usize>>::new();
    for at in &join.only_pit {
        let mutant = &pit[*at];
        let file = texts
            .keys()
            .find(|file| file.ends_with(&mutant.file))
            .cloned();
        let category = supercov_engine::jvm_strong::pit_only_category(
            mutant,
            file.as_ref()
                .and_then(|file| texts.get(file))
                .map(String::as_str),
        );
        *only_pit
            .entry(category.to_owned())
            .or_default()
            .entry(mutant.mutator.clone())
            .or_default() += 1;
    }
    let mut only_supercov = BTreeMap::<String, usize>::new();
    for id in &join.only_supercov {
        let mutator = id.rsplit(':').nth(1).unwrap_or("").to_owned();
        *only_supercov.entry(mutator).or_default() += 1;
    }
    let only_pit_list = join
        .only_pit
        .iter()
        .take(200)
        .map(|at| {
            let mutant = &pit[*at];
            json!({"file": mutant.file, "line": mutant.line, "mutator": mutant.mutator, "description": mutant.description, "status": mutant.status})
        })
        .collect::<Vec<_>>();
    Ok(json!({
        "run": run.id,
        "pit": pit.len(),
        "supercov": ours.values().map(Vec::len).sum::<usize>(),
        "matched": join.pairs.len(),
        "agree": agree,
        "notCompared": not_compared,
        "onlyPit": { "total": join.only_pit.len(), "byCategory": only_pit, "mutants": only_pit_list },
        "onlySupercov": { "total": join.only_supercov.len(), "byMutator": only_supercov, "mutants": join.only_supercov.iter().take(200).collect::<Vec<_>>() },
        "soundnessViolations": { "total": violations.len(), "mutants": violations },
    }))
}

fn text(data: &Value) -> String {
    let mut out = format!(
        "PIT's {} mutants beside the run's {}: {} matched, {} agree, {} soundness violation(s)\n",
        data["pit"],
        data["supercov"],
        data["matched"],
        data["agree"],
        data["soundnessViolations"]["total"]
    );
    out.push_str(&format!("  only PIT        {}\n", data["onlyPit"]["total"]));
    if let Some(categories) = data["onlyPit"]["byCategory"].as_object() {
        for (category, mutators) in categories {
            let total = mutators
                .as_object()
                .map_or(0, |m| m.values().filter_map(Value::as_u64).sum::<u64>());
            out.push_str(&format!("    {total:>5}  {category}\n"));
        }
    }
    out.push_str(&format!(
        "  only Supercov   {}\n",
        data["onlySupercov"]["total"]
    ));
    if let Some(compared) = data["notCompared"].as_object() {
        for (pair, count) in compared {
            out.push_str(&format!("  not compared    {count} ({pair})\n"));
        }
    }
    for violation in data["soundnessViolations"]["mutants"]
        .as_array()
        .into_iter()
        .flatten()
    {
        out.push_str(&format!(
            "  violation  {}  supercov {} / pit {}\n",
            violation["id"].as_str().unwrap_or(""),
            violation["status"].as_str().unwrap_or(""),
            violation["pit"].as_str().unwrap_or("")
        ));
    }
    out
}
