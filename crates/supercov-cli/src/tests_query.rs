//! `supercov runs <run> tests affected`: which of a run's tests the changes
//! since that run could have reached.

use serde_json::{Value, json};
use std::{collections::BTreeSet, path::Path, process::ExitCode};
use supercov_engine::{
    agent_json,
    run_store::{StoredRun, compare_run_integrity, discover_runs, select_run},
    source_manifest as maps,
};

const HELP: &str = r#"Usage: supercov runs <run> tests affected [--json | --names | --files] [--ran-changed | --asserting]

Which of the run's tests the changes since that run could have reached, from
what each test executed and what changed, declaration by declaration.

A test is affected by a change in code it ran, in its test file, or in the
shape of a file it ran code in -- a declaration added, removed or renamed. It
is not affected by a change confined to code it never ran, nor by comments,
blank lines or trailing whitespace. A test that did not pass in the run is
listed as affected: it has a result to establish.

A dependency, lockfile, configuration or toolchain change affects every test.
A source file added since the run is not seen by any test's record; the
working-tree check names that case and the suite should run in full.

When the run has been assessed (`supercov runs <run> assertions assess`), each
affected test also says whether it was judged to catch a change to the code
that changed -- it asserts what those statements compute -- and those tests
are listed first. Without an assessment the answer is coverage alone.

--json       Structured output for integrations.
--names      One affected test name per line, for a runner's filter.
--files      One affected test file per line, for a runner's file list.
--ran-changed  With --names or --files, after `assertions assess --changed`:
               leave out the tests that ran the changed declaration but none
               of the statements that changed. Coverage says they could not
               have seen the change.
--asserting    Also leave out the tests that ran the changed statements but
               were judged to catch no change to them. Smaller, and a
               judgment: over random changes it kept 92-95% of the tests a
               change broke, where --ran-changed kept all of them.

With an assessment, --names and --files list the tests most likely to catch
the change first. A test whose own file changed or that did not pass, and any
test never asked, always stays. Without an assessment both narrowing options
list every affected test.

Exit 0 when the run has a record to answer from; exit 2 otherwise.
"#;

/// Working-tree differences the per-test record cannot see: every test is
/// affected by them.
const RUN_WIDE: [&str; 6] = [
    "coverage schema changed",
    "instrumenter contract changed",
    "dependencies or lockfile changed",
    "test/build configuration changed",
    "execution environment changed",
    "run predates integrity fingerprints",
];

pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h"))
        || args.get(2).map(String::as_str) != Some("affected")
    {
        print!("{HELP}");
        return if args
            .get(2)
            .is_some_and(|a| a != "affected" && !a.starts_with('-'))
        {
            eprintln!("[supercov] unknown tests action: {}", args[2]);
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        };
    }
    let mut json_output = false;
    let mut names = false;
    let mut files = false;
    let mut asserting = false;
    let mut ran_changed = false;
    let mut seen = BTreeSet::new();
    for flag in &args[3..] {
        if !seen.insert(flag.as_str()) {
            return usage(format!("Duplicate option: {flag}"));
        }
        match flag.as_str() {
            "--json" => json_output = true,
            "--names" => names = true,
            "--files" => files = true,
            "--asserting" => asserting = true,
            "--ran-changed" => ran_changed = true,
            other => return usage(format!("Unknown option: {other}")),
        }
    }
    if [json_output, names, files].iter().filter(|f| **f).count() > 1 {
        return usage("Choose one of --json, --names or --files".into());
    }
    if (asserting || ran_changed) && !(names || files) {
        return usage("--asserting and --ran-changed narrow --names or --files".into());
    }
    if asserting && ran_changed {
        return usage("Choose one of --ran-changed or --asserting".into());
    }
    let result = (|| -> Result<Value, String> {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
        let run = select_run(&inventory, Some(&args[0])).map_err(|e| e.to_string())?;
        let mut data = maps::affected_tests(&root, run)?;
        data["workingTree"] = working_tree(&root, run, &mut data);
        consult_assertions(&root, run, &mut data);
        Ok(data)
    })();
    match result {
        Ok(data) => {
            if json_output {
                match fitted(data) {
                    Ok(output) => print!("{output}"),
                    Err(size) => {
                        print!(
                            "{}",
                            agent_json::failure(
                                Some("runs.tests.affected"),
                                &agent_json::AgentError {
                                    code: agent_json::ErrorCode::ResponseTooLarge,
                                    message: "Response too large; use the text view (omit --json)"
                                        .into(),
                                    retryable: false,
                                    details: Some(
                                        json!({"actualBytes": size.actual_bytes, "maxBytes": size.max_bytes})
                                    ),
                                }
                            )
                        );
                        return ExitCode::from(2);
                    }
                }
            } else if names || files {
                let key = if names { "name" } else { "file" };
                let available = data["assertions"]["available"] == true;
                if (asserting || ran_changed) && !available {
                    eprintln!(
                        "[supercov] this run has no assertion assessment; listing every affected test"
                    );
                }
                let dropped: &[&str] = match (available, asserting, ran_changed) {
                    (true, true, _) => &["runs", "misses"],
                    (true, _, true) => &["misses"],
                    _ => &[],
                };
                // Both buckets. This output feeds a runner, so it has to be
                // the set that is safe to run rather than the set that is
                // proven affected: a test whose coverage nothing could
                // attribute is exactly the one a narrower list would drop, and
                // dropping it is how a change to code it exercised goes
                // untested while the command exits 0.
                let mut lines = ["affected", "undetermined"]
                    .iter()
                    .flat_map(|bucket| data[*bucket].as_array().into_iter().flatten())
                    // Only a test asked about the changed statements and judged
                    // to catch none of them is left out: one never asked is
                    // no evidence either way.
                    .filter(|t| {
                        !t["assertion"]["verdict"]
                            .as_str()
                            .is_some_and(|v| dropped.contains(&v))
                    })
                    .filter_map(|t| t[key].as_str())
                    .collect::<Vec<_>>();
                // Ranked when there is an assessment (most likely to catch
                // the change first), sorted otherwise.
                if !available {
                    lines.sort_unstable();
                }
                let mut seen = BTreeSet::new();
                lines.retain(|line| seen.insert(*line));
                for line in lines {
                    println!("{line}");
                }
            } else {
                print!("{}", render(&data));
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            if json_output {
                print!(
                    "{}",
                    agent_json::failure(
                        Some("runs.tests.affected"),
                        &agent_json::AgentError {
                            code: agent_json::ErrorCode::InvalidArgument,
                            message,
                            retryable: false,
                            details: None,
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

/// What the run's assertion assessment says about each affected test: whether
/// it was judged to catch a change to the statements that changed in code it
/// ran. Without an assessment the answer stays coverage alone.
///
/// The statements considered are the run's statements in each changed
/// declaration's own lines; of those, the ones whose first line no longer
/// appears in the declaration now are the edited ones, and they are preferred
/// when there are any. A test is judged to catch the change when its own
/// answer for one of them is at least the threshold; judged not to when it was
/// asked about some and caught none; unassessed when it was asked about none.
fn consult_assertions(root: &Path, run: &StoredRun, data: &mut Value) {
    let Some(saved) = super::assertions_command::saved(run) else {
        data["assertions"] = json!({
            "available": false,
            "reason": "This run has not been assessed; affected tests come from coverage alone. `supercov runs <run> assertions assess` adds which of them check the changed code.",
        });
        strip_changed_code(data);
        return;
    };
    let impact = super::assertions_command::impact(run);
    apply_assertions(&saved, impact.as_ref(), data, &mut CurrentLines::new(root));
}

/// [`consult_assertions`] once the run's assessment and impact answers are
/// read: verdicts, order and summary written into `data`.
fn apply_assertions(
    saved: &Value,
    impact: Option<&Value>,
    data: &mut Value,
    current: &mut CurrentLines,
) {
    let threshold = supercov_engine::assertion_coverage::THRESHOLD;
    let by_file = merged_statements(saved, impact);
    let mut changed_statements = |code: &Value| -> Vec<Value> {
        let file = code["file"].as_str().unwrap_or("");
        let candidates = by_file.get(file).cloned().unwrap_or_default();
        in_changed_code(current, code, candidates)
    };
    let mut changed_all = std::collections::BTreeMap::<(String, u64), Value>::new();
    let mut ranked = Vec::new();
    for mut test in data["affected"].as_array().cloned().unwrap_or_default() {
        let name = test["name"].as_str().unwrap_or("").to_owned();
        let mut answered = Vec::new();
        let (mut in_play, mut all_complete) = (0usize, true);
        for code in test["changedCode"].as_array().cloned().unwrap_or_default() {
            for statement in changed_statements(&code) {
                let key = (
                    statement["file"].as_str().unwrap_or("").to_owned(),
                    statement["line"].as_u64().unwrap_or(0),
                );
                changed_all.entry(key).or_insert_with(|| statement.clone());
                in_play += 1;
                all_complete &= statement["complete"] == true;
                let mine = statement["answers"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .find(|a| a["name"].as_str() == Some(name.as_str()))
                    .and_then(|a| a["p"].as_f64());
                if let Some(p) = mine {
                    answered.push(json!({
                        "file": statement["file"], "line": statement["line"],
                        "text": statement["text"], "p": p,
                    }));
                }
            }
        }
        let best = answered
            .iter()
            .filter_map(|a| a["p"].as_f64())
            .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))));
        let own_change = test["reasons"].as_array().into_iter().flatten().any(|r| {
            r.as_str()
                .is_some_and(|r| r.starts_with("test file") || r == "did not pass in the run")
        });
        let (verdict, rank) = match best {
            _ if own_change => ("must run", 0),
            Some(p) if p >= threshold => ("catches", 1),
            // Every test that ran the changed statements was asked, and this
            // one was not: it ran the changed declaration, not what changed.
            None if in_play > 0 && all_complete => ("misses", 4),
            None => ("unassessed", 2),
            Some(_) => ("runs", 3),
        };
        answered.sort_by(|a, b| {
            b["p"]
                .as_f64()
                .partial_cmp(&a["p"].as_f64())
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        answered.truncate(3);
        test["assertion"] = json!({"verdict": verdict, "best": best, "statements": answered});
        ranked.push((
            rank,
            std::cmp::Reverse((best.unwrap_or(0.0) * 1000.0) as i64),
            test,
        ));
    }
    // A test with a change of its own first; then the name match the
    // coverage order already uses, which predicted failing tests better than
    // the verdicts on replayed commits; the verdicts break its ties.
    ranked.sort_by(|a, b| {
        let key = |r: &(u8, std::cmp::Reverse<i64>, Value)| {
            (
                r.0 != 0,
                std::cmp::Reverse(r.2["nameMatch"].as_u64().unwrap_or(0)),
                r.0,
                r.1,
            )
        };
        (key(a), a.2["file"].as_str(), a.2["name"].as_str()).cmp(&(
            key(b),
            b.2["file"].as_str(),
            b.2["name"].as_str(),
        ))
    });
    let counts = |v: &str| {
        ranked
            .iter()
            .filter(|(_, _, t)| t["assertion"]["verdict"] == v)
            .count()
    };
    let summary = json!({
        "catches": counts("catches"), "runs": counts("runs"), "misses": counts("misses"),
        "unassessed": counts("unassessed"), "mustRun": counts("must run"),
    });
    data["affected"] = json!(ranked.into_iter().map(|(_, _, t)| t).collect::<Vec<_>>());
    strip_changed_code(data);
    let unasserted = changed_all
        .values()
        .filter(|s| s["asserted"] != true)
        .map(|s| json!({"file": s["file"], "line": s["line"], "text": s["text"]}))
        .collect::<Vec<_>>();
    data["assertions"] = json!({
        "available": true,
        "model": saved["model"],
        "impact": impact.is_some(),
        "tests": summary,
        "changedStatements": changed_all.len(),
        "assertedStatements": changed_all.len() - unasserted.len(),
        "unasserted": unasserted,
        "meaning": "Of the statements in changed code the run assessed, which some test is judged to catch a change to; per test, whether it is judged to catch one in the code it ran. Ordered: tests that must run, tests that catch the change, unassessed ones, tests that run the change but are not judged to catch it, then tests that ran the changed declaration but none of the changed statements.",
    });
}

/// Which statements, in order, still stand among the current lines: the
/// longest run of them matching lines in the same order. Order matters, since
/// the same line (`return None`) can appear twice in one function.
fn aligned(statements: &[&str], lines: &[String]) -> Vec<bool> {
    let (n, m) = (statements.len(), lines.len());
    if n * m > 4_000_000 {
        return statements
            .iter()
            .map(|s| lines.iter().any(|l| l == s))
            .collect();
    }
    let mut table = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[i][j] = if statements[i] == lines[j] {
                table[i + 1][j + 1] + 1
            } else {
                table[i + 1][j].max(table[i][j + 1])
            };
        }
    }
    let mut kept = vec![false; n];
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if statements[i] == lines[j] {
            kept[i] = true;
            i += 1;
            j += 1;
        } else if table[i + 1][j] >= table[i][j + 1] {
            i += 1;
        } else {
            j += 1;
        }
    }
    kept
}

/// The run's assessed statements by file, each with every answer known for
/// it: the assessment's, and those `assertions assess --changed` added for
/// changed code (asked of every test that ran it).
fn merged_statements(
    saved: &Value,
    impact: Option<&Value>,
) -> std::collections::BTreeMap<String, Vec<Value>> {
    let mut extra = std::collections::BTreeMap::<(String, u64), Vec<Value>>::new();
    let mut complete = BTreeSet::<(String, u64)>::new();
    for statement in impact
        .and_then(|i| i["statements"].as_array())
        .into_iter()
        .flatten()
    {
        let key = (
            statement["file"].as_str().unwrap_or("").to_owned(),
            statement["line"].as_u64().unwrap_or(0),
        );
        extra
            .entry(key.clone())
            .or_default()
            .extend(statement["answers"].as_array().cloned().unwrap_or_default());
        if statement["complete"] == true {
            complete.insert(key);
        }
    }
    let mut by_file = std::collections::BTreeMap::<String, Vec<Value>>::new();
    for statement in saved["statements"].as_array().into_iter().flatten() {
        let mut statement = statement.clone();
        let key = (
            statement["file"].as_str().unwrap_or("").to_owned(),
            statement["line"].as_u64().unwrap_or(0),
        );
        if complete.contains(&key) {
            statement["complete"] = json!(true);
        }
        if let Some(more) = extra.remove(&key) {
            let mut answers = statement["answers"].as_array().cloned().unwrap_or_default();
            for answer in more {
                if !answers.iter().any(|a| a["name"] == answer["name"]) {
                    answers.push(answer);
                }
            }
            if answers.iter().any(|a| {
                a["p"]
                    .as_f64()
                    .is_some_and(|p| p >= supercov_engine::assertion_coverage::THRESHOLD)
            }) {
                statement["asserted"] = json!(true);
            }
            statement["answers"] = json!(answers);
        }
        by_file.entry(key.0).or_default().push(statement);
    }
    by_file
}

/// Current file lines, trimmed, read once each.
pub(crate) struct CurrentLines<'r> {
    root: &'r Path,
    files: std::collections::BTreeMap<String, Vec<String>>,
}

impl<'r> CurrentLines<'r> {
    pub(crate) fn new(root: &'r Path) -> Self {
        CurrentLines {
            root,
            files: Default::default(),
        }
    }
    fn get(&mut self, file: &str) -> &[String] {
        let root = self.root;
        self.files.entry(file.to_owned()).or_insert_with(|| {
            std::fs::read_to_string(root.join(file))
                .unwrap_or_default()
                .lines()
                .map(|l| l.trim().to_owned())
                .collect()
        })
    }
}

/// Of a file's statements (each with `line` and `text`), those a changed
/// declaration puts in play: the ones in its own lines in the run, narrowed
/// to those no longer standing in it now (matched in order) when there are
/// any.
pub(crate) fn in_changed_code(
    current: &mut CurrentLines,
    code: &Value,
    statements: Vec<Value>,
) -> Vec<Value> {
    let file = code["file"].as_str().unwrap_or("");
    let own = code["own"].as_array().cloned().unwrap_or_default();
    let inside = |line: u64| {
        own.iter().any(|r| {
            r[0].as_u64().is_some_and(|a| a <= line) && r[1].as_u64().is_some_and(|b| line <= b)
        })
    };
    let mut candidates = statements
        .into_iter()
        .filter(|s| s["line"].as_u64().is_some_and(inside))
        .collect::<Vec<_>>();
    candidates.sort_by_key(|s| s["line"].as_u64());
    let Some(span) = code["now"].as_array() else {
        return candidates;
    };
    let (a, b) = (
        span[0].as_u64().unwrap_or(1) as usize,
        span[1].as_u64().unwrap_or(0) as usize,
    );
    let now = current.get(file);
    let lines = now
        .get(a.saturating_sub(1)..b.min(now.len()))
        .unwrap_or(&[]);
    let texts = candidates
        .iter()
        .map(|s| s["text"].as_str().unwrap_or("").trim())
        .collect::<Vec<_>>();
    let kept = aligned(&texts, lines);
    let edited = candidates
        .iter()
        .zip(kept)
        .filter(|(_, kept)| !kept)
        .map(|(s, _)| s.clone())
        .collect::<Vec<_>>();
    if edited.is_empty() {
        candidates
    } else {
        edited
    }
}

/// The JSON envelope for `data`, made to fit one response: on a large suite
/// the unaffected tests become a count, then each test keeps its first three
/// reasons, then the lists keep their ranked head -- each step saying so, and
/// `--names` always lists every test.
fn fitted(mut data: Value) -> Result<String, agent_json::ResponseTooLarge> {
    let mut step = 0;
    loop {
        match agent_json::success("runs.tests.affected", &data, None) {
            Ok(output) => return Ok(output),
            Err(size) => {
                let mut notes = Vec::new();
                match step {
                    0 => {
                        let count = data["unaffected"].as_array().map_or(0, Vec::len);
                        data["unaffected"] = json!([]);
                        data["unaffectedCount"] = json!(count);
                        notes.push("unaffected tests are counted, not listed");
                    }
                    1 => {
                        for bucket in ["affected", "undetermined"] {
                            for test in data[bucket].as_array_mut().into_iter().flatten() {
                                if let Some(reasons) = test["reasons"].as_array_mut() {
                                    reasons.truncate(3);
                                }
                            }
                        }
                        notes.push("each test keeps its first three reasons");
                    }
                    _ => {
                        let mut shortened = false;
                        for bucket in ["undetermined", "affected"] {
                            let listed = data[bucket].as_array().map_or(0, Vec::len);
                            if listed > 0 {
                                let total = data[format!("{bucket}Count")]
                                    .as_u64()
                                    .unwrap_or(listed as u64);
                                data[format!("{bucket}Count")] = json!(total);
                                data[bucket] =
                                    json!(data[bucket].as_array().unwrap()[..listed / 2]);
                                shortened = true;
                                break;
                            }
                        }
                        if !shortened {
                            return Err(size);
                        }
                        notes.push("the lists keep their first, highest-ranked tests; `--names` lists every one");
                    }
                }
                step += 1;
                let mut all = data["truncated"].as_array().cloned().unwrap_or_default();
                for note in notes {
                    if !all.iter().any(|n| n == note) {
                        all.push(json!(note));
                    }
                }
                data["truncated"] = json!(all);
            }
        }
    }
}

/// The changed declarations each test ran are what the assertions are read
/// against; once read they are detail no reader needs, and on a large suite
/// they are most of the response.
fn strip_changed_code(data: &mut Value) {
    for bucket in ["affected", "undetermined", "unaffected"] {
        for test in data[bucket].as_array_mut().into_iter().flatten() {
            if let Some(entry) = test.as_object_mut() {
                entry.remove("changedCode");
            }
        }
    }
}

fn usage(message: String) -> ExitCode {
    eprintln!("[supercov] {message}");
    print!("{HELP}");
    ExitCode::from(2)
}

/// The checkout against the run, and what that means for the answer: a
/// run-wide difference makes every test affected; a source set that changed
/// without any captured file changing means a file was added.
fn working_tree(root: &Path, run: &StoredRun, data: &mut Value) -> Value {
    let Some(current) = super::current_integrity_for_run(root, run) else {
        return json!({"stale": null, "reason": "current source integrity unavailable"});
    };
    let comparison = compare_run_integrity(Some(&run.metadata.integrity), &current);
    let run_wide = comparison
        .reasons
        .iter()
        .filter(|r| RUN_WIDE.contains(&r.as_str()))
        .cloned()
        .collect::<Vec<_>>();
    if !run_wide.is_empty() {
        let reason = format!("{} (every test)", run_wide.join("; "));
        let mut all = data["affected"].as_array().cloned().unwrap_or_default();
        for bucket in ["undetermined", "unaffected"] {
            for test in data[bucket].as_array().cloned().unwrap_or_default() {
                all.push(test);
            }
        }
        for test in &mut all {
            let reasons = test["reasons"].as_array_mut().unwrap();
            reasons.insert(0, json!(reason));
        }
        all.sort_by(|a, b| {
            (a["file"].as_str(), a["name"].as_str()).cmp(&(b["file"].as_str(), b["name"].as_str()))
        });
        data["summary"]["affected"] = json!(all.len());
        data["summary"]["undetermined"] = json!(0);
        data["summary"]["unaffected"] = json!(0);
        data["affected"] = json!(all);
        data["undetermined"] = json!([]);
        data["unaffected"] = json!([]);
    }
    let source_set_changed = comparison
        .reasons
        .iter()
        .any(|r| r == "instrumented source changed" || r == "test files changed")
        && data["changedFiles"].as_array().is_some_and(Vec::is_empty);
    json!({
        "stale": comparison.stale,
        "reasons": comparison.reasons,
        "runWide": run_wide,
        "sourceSetChanged": source_set_changed,
        "note": if source_set_changed {
            Some("A source or test file exists that the run did not capture; no test's record can say whether it loads it. Run the suite in full.")
        } else {
            None
        },
    })
}

fn render(data: &Value) -> String {
    let mut out = String::new();
    let affected = data["affected"].as_array().cloned().unwrap_or_default();
    let undetermined = data["undetermined"].as_array().cloned().unwrap_or_default();
    let total = data["summary"]["tests"].as_u64().unwrap_or(0);
    out.push_str(&format!(
        "Run {}: {} of {} tests affected by changes since the run{}\n",
        data["run"].as_str().unwrap_or(""),
        affected.len(),
        total,
        if undetermined.is_empty() {
            String::new()
        } else {
            format!(", {} undetermined", undetermined.len())
        }
    ));
    fn entries(out: &mut String, tests: &[Value]) {
        for test in tests {
            out.push_str(&format!(
                "\n  {} › {}\n",
                test["file"].as_str().unwrap_or(""),
                test["name"].as_str().unwrap_or("")
            ));
            for reason in test["reasons"].as_array().into_iter().flatten() {
                out.push_str(&format!("    {}\n", reason.as_str().unwrap_or("")));
            }
            let assertion = &test["assertion"];
            match assertion["verdict"].as_str() {
                Some("catches") => {
                    let top = &assertion["statements"][0];
                    out.push_str(&format!(
                        "    catches a change to {}:{} ({:.2})\n",
                        top["file"].as_str().unwrap_or(""),
                        top["line"],
                        top["p"].as_f64().unwrap_or(0.0)
                    ));
                }
                Some("runs") => {
                    out.push_str("    runs the changed code; not judged to catch a change to it\n")
                }
                Some("unassessed") => out.push_str("    not assessed against the changed code\n"),
                Some("misses") => out
                    .push_str("    ran the changed declaration, not the statements that changed\n"),
                _ => {}
            }
        }
    }
    let assertions = &data["assertions"];
    if assertions["available"] == true {
        let changed = assertions["changedStatements"].as_u64().unwrap_or(0);
        if changed > 0 {
            out.push_str(&format!(
                "Assertions: {} of {} changed statements the run assessed are asserted by a test; {} affected test(s) catch the change.\n",
                assertions["assertedStatements"].as_u64().unwrap_or(0),
                changed,
                assertions["tests"]["catches"].as_u64().unwrap_or(0),
            ));
            for s in assertions["unasserted"]
                .as_array()
                .into_iter()
                .flatten()
                .take(10)
            {
                out.push_str(&format!(
                    "  not asserted: {}:{}  {}\n",
                    s["file"].as_str().unwrap_or(""),
                    s["line"],
                    s["text"].as_str().unwrap_or("")
                ));
            }
        }
    }
    entries(&mut out, &affected);
    if !undetermined.is_empty() {
        out.push_str(
            "\nUndetermined — what these ran was not fully recorded, so nothing can say the\nchange missed them. It is inside what the run covered, so it could have reached\nthem. Run them.\n",
        );
        entries(&mut out, &undetermined);
    }
    let files = data["changedFiles"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        out.push_str("\nNo captured file has changed.\n");
    } else {
        out.push_str("\nChanged files:\n");
        for file in &files {
            out.push_str(&format!(
                "  {}  {}{}\n",
                file["file"].as_str().unwrap_or(""),
                file["change"].as_str().unwrap_or(""),
                file["detail"]
                    .as_str()
                    .map(|d| format!("  {d}"))
                    .unwrap_or_default()
            ));
        }
    }
    let tree = &data["workingTree"];
    match tree["stale"] {
        Value::Bool(false) => out.push_str("\nWorking tree: matches the run.\n"),
        Value::Bool(true) => {
            out.push_str("\nWorking tree: differs from the run:\n");
            for reason in tree["reasons"].as_array().into_iter().flatten() {
                out.push_str(&format!("  {}\n", reason.as_str().unwrap_or("")));
            }
            if let Some(note) = tree["note"].as_str() {
                out.push_str(&format!("  {note}\n"));
            }
        }
        _ => out.push_str("\nWorking tree: could not be compared with the run.\n"),
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// dry-rb/dry-inflector, 1,128 examples, a change every test reached:
    /// 364 KB of JSON and the command failed instead of answering.
    #[test]
    fn a_large_affected_set_is_shortened_to_fit_one_response() {
        let test = |i: usize| json!({"file": "spec/unit/dry/inflector/pluralize_spec.rb", "name": format!("spec/unit/dry/inflector/pluralize_spec.rb[1:1:{i}]"), "reasons": ["lib/dry/inflector.rb: Inflector#pluralize (line 120) changed (this test ran code in this file)", "lib/dry/inflector/inflections.rb: top level changed", "lib/dry/inflector/rules.rb: Rules#apply_to (line 30) changed", "a fourth reason that is dropped"]});
        let data = json!({
            "affected": (0..1100).map(test).collect::<Vec<_>>(),
            "unaffected": (0..1100).map(test).collect::<Vec<_>>(),
            "undetermined": [],
        });
        let output = fitted(data).expect("shortened to fit");
        let parsed: Value = serde_json::from_str(&output).unwrap();
        let d = &parsed["data"];
        assert_eq!(d["unaffectedCount"], 1100);
        assert_eq!(d["affectedCount"], 1100);
        assert!(d["affected"].as_array().unwrap().len() < 1100);
        assert!(d["affected"][0]["reasons"].as_array().unwrap().len() <= 3);
        assert_eq!(
            d["affected"][0]["name"],
            "spec/unit/dry/inflector/pluralize_spec.rb[1:1:0]"
        );
        assert!(d["truncated"].as_array().unwrap().len() >= 2);
    }

    #[test]
    fn affected_tests_are_ranked_by_whether_they_catch_the_change() {
        let root = std::env::temp_dir().join(format!(
            "supercov-tests-affected-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(root.join("src")).unwrap();
        // Line 2 changed (0 -> 1); line 3 is the same.
        std::fs::write(
            root.join("src/a.py"),
            "def f(x):\n    y = x + 1\n    return y\n",
        )
        .unwrap();
        let code =
            json!([{"file": "src/a.py", "declaration": "f", "own": [[1, 3]], "now": [1, 3]}]);
        let test = |name: &str| json!({"file": "tests/t.py", "name": name, "reasons": ["src/a.py: f (line 1) changed (this test ran it)"], "changedCode": code});
        let mut data = json!({
            "affected": [test("runs"), test("misses"), test("catches"), test("unasked")],
            "summary": {"tests": 4},
        });
        let saved = json!({"model": "m", "statements": [
            {"file": "src/a.py", "line": 2, "text": "y = x + 0", "asserted": true,
             "answers": [{"name": "catches", "p": 0.9}, {"name": "runs", "p": 0.2}]},
            {"file": "src/a.py", "line": 3, "text": "return y", "asserted": true,
             "answers": [{"name": "unasked", "p": 0.9}]},
        ]});
        // Without impact answers the unasked test stays unassessed.
        let mut plain = data.clone();
        apply_assertions(&saved, None, &mut plain, &mut CurrentLines::new(&root));
        let verdicts = |d: &Value| {
            d["affected"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    (
                        t["name"].as_str().unwrap().to_owned(),
                        t["assertion"]["verdict"].as_str().unwrap().to_owned(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            verdicts(&plain),
            vec![
                ("catches".into(), "catches".into()),
                ("misses".into(), "unassessed".into()),
                ("unasked".into(), "unassessed".into()),
                ("runs".into(), "runs".into()),
            ]
        );
        // Only the edited statement (line 2) is in play: the answer for the
        // unchanged line 3 does not count.
        assert_eq!(plain["assertions"]["changedStatements"], 1);
        // With every runner of line 2 asked, a test without an answer did not
        // run it.
        let impact = json!({"statements": [{"file": "src/a.py", "line": 2, "complete": true, "answers": []}]});
        apply_assertions(
            &saved,
            Some(&impact),
            &mut data,
            &mut CurrentLines::new(&root),
        );
        let v = verdicts(&data);
        assert!(v.contains(&("misses".into(), "misses".into())), "{v:?}");
        assert!(v.contains(&("unasked".into(), "misses".into())), "{v:?}");
        assert_eq!(v[0], ("catches".into(), "catches".into()));
        assert!(
            data["affected"][0].get("changedCode").is_none(),
            "detail is dropped once read"
        );
        std::fs::remove_dir_all(&root).ok();
    }

    #[test]
    fn statements_are_matched_in_order() {
        let lines = ["if a:", "return 1", "x = 2", "return 1"].map(String::from);
        // The first `return 1` was edited; the second still stands.
        let run = ["if a:", "return 0", "x = 2", "return 1"];
        assert_eq!(aligned(&run, &lines), vec![true, false, true, true]);
    }

    #[test]
    fn the_text_view_names_each_affected_test_and_its_reasons() {
        let data = json!({
            "run": "r1",
            "summary": {"tests": 3},
            "affected": [{"file": "tests/a.test.js", "name": "adds", "reasons": ["src/a.js: add (line 1) changed (this test ran it)"]}],
            "changedFiles": [{"file": "src/a.js", "change": "bodies", "detail": "add (line 1)"}],
            "workingTree": {"stale": false}
        });
        let text = render(&data);
        assert!(text.contains("1 of 3 tests affected"), "{text}");
        assert!(text.contains("tests/a.test.js › adds"), "{text}");
        assert!(
            text.contains("    src/a.js: add (line 1) changed (this test ran it)"),
            "{text}"
        );
        assert!(text.contains("  src/a.js  bodies  add (line 1)"), "{text}");
        assert!(text.contains("matches the run"), "{text}");
    }
}
