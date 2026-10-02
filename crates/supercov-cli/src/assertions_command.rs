//! `supercov runs <run> assertions`: which executed statements the tests would
//! catch breaking, assessed by Jev from what each test ran.

use crate::quality;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    process::ExitCode,
};
use supercov_engine::{
    agent_json,
    assertion_coverage::{
        self as coverage, Change, FIRST_TESTS, MAX_QUESTIONS, MORE_TESTS, Population, THRESHOLD,
    },
    run_store::{StoredRun, discover_runs, select_run},
    source_manifest as maps,
};

const HELP: &str = r#"Usage: supercov runs <run> assertions [<file> | <file>:<line> | --test <name>] [--offset N] [--limit N | --all] [--json]
       supercov runs <run> assertions assess [--changed] [--dry-run] [--workers N] [--json]

How much of the executed code the tests assert: a statement counts as asserted
when changing it would make at least one passing test that runs it fail. The
change follows the statement: an `if` inverted (each way), `return x`
returning undefined, a declaration's value becoming undefined, anything else
skipped. Imports and declarations without behaviour are left out.

`assertions` reads the run's saved result: no network, no key.

`assertions assess` works it out. For each test, Supercov renders what the test
ran and asks Jev whether the test would fail for each statement, then saves the
result to the run. Answers are kept in .supercov/assertions/ and reused only
when a question would be asked with exactly the same text, so after a change
only the affected questions are asked again. Needs TYPESAFE_API_KEY for
questions not yet answered. JavaScript, TypeScript, Python, Ruby, Go, Rust, Java and Kotlin runs.

Reading the saved result goes from the run down, a page at a time:
  assertions                the share asserted, and the files by how many of
                            their statements are not asserted
  assertions <file>         that file's statements that are not asserted, each
                            with its change and the test that came closest
  assertions <file>:<line>  one statement: its change and every asked test's
                            answer
  assertions --test <name>  the statements one test was asked about, and which
                            of them it was judged to catch

--offset N    Start the listing at its Nth entry (default 0).
--limit N     List N entries (default 20).
--all         List every entry.
--changed     (assess) Only the statements in code changed since the run,
              asked of every test that ran them rather than until one
              catches the change: what `tests affected` reads to say which
              tests check the change. Saved beside the run's assessment.
--dry-run     (assess) Estimate the requests and cost; send nothing.
--workers N   (assess) Requests in flight at once (default 24). Each round
              plans the same requests whatever this is, so it changes only
              how long the pass takes.
--json        Structured output for integrations.
"#;

const CACHE_VERSION: u32 = 1;
/// Consecutive failed requests after which the pass stops rather than keep
/// spending: an exhausted account or an outage fails every request alike.
const MAX_FAILURES: usize = 20;

/// Requests planned per round of the assessment pass.
const ROUND_REQUESTS: usize = 40;

/// Jev 1.13.0 takes 32k tokens of state plus the longest question and 64k of
/// state plus every question; these leave room for the estimate to be low.
const STATE_AND_QUESTION_TOKENS: usize = 30_000;
const STATE_AND_QUESTIONS_TOKENS: usize = 60_000;

fn tokens(value: &Value) -> usize {
    quality::estimated_tokens(&serde_json::to_vec(value).unwrap_or_default())
}

/// The planned requests, each within Jev's limits: one over them is split by
/// its questions, and a test whose request is over even with one question is
/// not asked about that statement. Supercov's own suite, whose tests hold
/// long inline fixtures, had every request refused and the pass stopped.
fn within_limits(
    population: &Population,
    pass: &mut Pass,
    batch: Vec<(usize, Vec<usize>)>,
) -> Vec<(usize, Vec<usize>)> {
    let mut out = Vec::new();
    let mut pending = batch;
    while let Some((t, asked)) = pending.pop() {
        let request = population.request(t, &asked, pass.model);
        let state = tokens(&request["state"]);
        let longest = request["questions"]
            .as_object()
            .into_iter()
            .flat_map(|q| q.values())
            .map(tokens)
            .max()
            .unwrap_or(0);
        if state + longest <= STATE_AND_QUESTION_TOKENS
            && tokens(&request) <= STATE_AND_QUESTIONS_TOKENS
        {
            out.push((t, asked));
        } else if asked.len() > 1 {
            let (left, right) = asked.split_at(asked.len() / 2);
            pending.push((t, left.to_vec()));
            pending.push((t, right.to_vec()));
        } else if let Some(&s) = asked.first() {
            pass.refused.insert((s, t));
        }
    }
    out
}

/// The file in a run's directory holding its assessed result.
pub const RESULT_FILE: &str = "assertion-coverage.json";
/// The file in a run's directory holding `assess --changed`'s answers.
pub const IMPACT_FILE: &str = "assertion-impact.json";
/// The files an assessment rendered, as they were: `assess --changed` asks
/// about the run's code after the checkout has moved on.
pub const SNAPSHOT_FILE: &str = "assertion-sources.json.gz";
/// Tests asked about one changed statement: every test that ran it, up to
/// this many, chosen as the whole-app pass orders them.
const IMPACT_TESTS: usize = 40;

struct Options {
    assess: bool,
    changed: bool,
    dry_run: bool,
    json: bool,
    target: Target,
    offset: usize,
    limit: Option<usize>,
    workers: usize,
}

/// Where in the saved result a reading looks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Overview,
    File(String),
    Line(String, u64),
    Test(String),
}

/// Entries a listing shows unless told otherwise, as the other `runs`
/// listings do.
const PAGE: usize = 20;

fn target_of(value: &str) -> Target {
    let value = value.trim_start_matches("./").replace('\\', "/");
    if let Some((file, line)) = value.rsplit_once(':')
        && let Ok(line) = line.parse::<u64>()
    {
        return Target::Line(file.to_owned(), line);
    }
    Target::File(value)
}

pub fn command(args: &[String]) -> ExitCode {
    if args.iter().any(|a| matches!(a.as_str(), "--help" | "-h")) {
        print!("{HELP}");
        return ExitCode::SUCCESS;
    }
    let options = match parse(&args[2..]) {
        Ok(o) => o,
        Err(message) => {
            eprintln!("[supercov] {message}");
            print!("{HELP}");
            return ExitCode::from(2);
        }
    };
    let result = (|| -> Result<Value, String> {
        let root = std::env::current_dir().map_err(|e| e.to_string())?;
        let inventory = discover_runs(&root).map_err(|e| e.to_string())?;
        let run = select_run(&inventory, Some(&args[0])).map_err(|e| e.to_string())?;
        if options.assess {
            assess(&root, run, &options)
        } else {
            read(run, &inventory.runs, &options)
        }
    })();
    match result {
        Ok(data) => {
            if options.json {
                match fitted_json(data) {
                    Ok(output) => print!("{output}"),
                    Err(size) => {
                        print!(
                            "{}",
                            agent_json::failure(
                                Some("runs.assertions"),
                                &agent_json::AgentError {
                                    code: agent_json::ErrorCode::ResponseTooLarge,
                                    message: "Response too large; use --limit or the text view"
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
            } else {
                print!("{}", render(&data));
            }
            ExitCode::SUCCESS
        }
        Err(message) => {
            if options.json {
                print!(
                    "{}",
                    agent_json::failure(
                        Some("runs.assertions"),
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

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        assess: false,
        changed: false,
        dry_run: false,
        json: false,
        target: Target::Overview,
        offset: 0,
        limit: Some(PAGE),
        workers: 24,
    };
    let mut i = 0;
    if args.first().is_some_and(|a| a == "assess") {
        o.assess = true;
        i = 1;
    }
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" | "--workers" | "--changed" if !o.assess => {
                return Err(format!("{} belongs to `assertions assess`", args[i]));
            }
            "--test" | "--offset" | "--limit" | "--all" if o.assess => {
                return Err(format!(
                    "{} reads a saved result: `assertions {}` after the assessment",
                    args[i], args[i]
                ));
            }
            "--test" => {
                let name = args
                    .get(i + 1)
                    .filter(|name| !name.is_empty())
                    .ok_or("--test needs a test name")?;
                if o.target != Target::Overview {
                    return Err("give a file, a file:line or --test, not more than one".into());
                }
                o.target = Target::Test(name.clone());
                i += 1;
            }
            "--offset" => {
                o.offset = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<usize>().ok())
                    .ok_or("--offset needs a number")?;
                i += 1;
            }
            value if !o.assess && !value.starts_with('-') => {
                if o.target != Target::Overview {
                    return Err("give a file, a file:line or --test, not more than one".into());
                }
                o.target = target_of(value);
            }
            "--dry-run" => o.dry_run = true,
            "--changed" => o.changed = true,
            "--json" => o.json = true,
            "--all" => o.limit = None,
            "--limit" | "--workers" => {
                let flag = args[i].clone();
                let value = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|v| *v > 0)
                    .ok_or_else(|| format!("{flag} needs a positive number"))?;
                if flag == "--limit" {
                    o.limit = Some(value)
                } else {
                    o.workers = value.min(64)
                }
                i += 1;
            }
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 1;
    }
    Ok(o)
}

/// Answers kept across runs, keyed by the exact question (see
/// `Population::pair_key`).
struct Cache {
    path: PathBuf,
    answers: BTreeMap<String, f64>,
}

impl Cache {
    fn load(root: &Path) -> Cache {
        let path = root
            .join(".supercov")
            .join("assertions")
            .join("answers.json");
        let answers = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|v| v["version"] == CACHE_VERSION)
            .and_then(|v| serde_json::from_value(v["answers"].clone()).ok())
            .unwrap_or_default();
        Cache { path, answers }
    }
    fn save(&self) -> Result<(), String> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        let bytes = serde_json::to_vec(&json!({"version": CACHE_VERSION, "answers": self.answers}))
            .map_err(|e| e.to_string())?;
        let temp = self.path.with_extension("json.tmp");
        std::fs::write(&temp, bytes).map_err(|e| e.to_string())?;
        std::fs::rename(&temp, &self.path).map_err(|e| e.to_string())
    }
}

struct Pass<'p> {
    population: &'p Population,
    model: &'static str,
    salt: String,
    /// Per statement: (test, answer) in the order asked.
    answers: Vec<Vec<(usize, f64)>>,
    orders: Vec<Vec<usize>>,
    wide: bool,
    keys: BTreeMap<(usize, usize), String>,
    /// (statement, test) pairs whose request is over Jev's limit even with
    /// that one question: the test is not asked about the statement.
    refused: BTreeSet<(usize, usize)>,
}

impl Pass<'_> {
    fn key(&mut self, statement: usize, test: usize) -> String {
        if let Some(k) = self.keys.get(&(statement, test)) {
            return k.clone();
        }
        let k = self
            .population
            .pair_key(statement, test, self.model, &self.salt);
        self.keys.insert((statement, test), k.clone());
        k
    }
    fn asserted(&self, statement: usize) -> bool {
        self.answers[statement].iter().any(|(_, p)| *p >= THRESHOLD)
    }
    fn limit(&self, statement: usize) -> usize {
        (if self.wide { MORE_TESTS } else { FIRST_TESTS })
            .min(self.population.statements[statement].tests.len())
    }
    fn undecided(&self, statement: usize) -> bool {
        let refused = self
            .refused
            .range((statement, 0)..=(statement, usize::MAX))
            .count();
        !self.asserted(statement) && self.answers[statement].len() + refused < self.limit(statement)
    }
    /// Cached answers first: a statement asked this exact question before is
    /// answered without a request.
    fn seed(&mut self, cache: &Cache) -> usize {
        let mut reused = 0;
        for s in 0..self.population.statements.len() {
            for t in self.orders[s].clone() {
                if self.answers[s].iter().any(|(x, _)| *x == t) {
                    continue;
                }
                let key = self.key(s, t);
                let Some(&p) = cache.answers.get(&key) else {
                    continue;
                };
                self.answers[s].push((t, p));
                reused += 1;
                if p >= THRESHOLD {
                    break;
                }
            }
        }
        reused
    }
    /// Before a statement is called not asserted, more of the tests that run
    /// it are asked.
    fn widen(&mut self) {
        self.wide = true;
        for s in 0..self.population.statements.len() {
            if !self.asserted(s) {
                self.orders[s] = self.population.order(s, MORE_TESTS);
            }
        }
    }
    /// The next requests: the tests that can answer the most undecided
    /// statements, each asked about all of them it ran.
    fn plan(&self, requests: usize) -> Vec<(usize, Vec<usize>)> {
        let mut eligible = BTreeMap::<usize, Vec<usize>>::new();
        for s in 0..self.population.statements.len() {
            if !self.undecided(s) {
                continue;
            }
            for &t in &self.orders[s] {
                if !self.answers[s].iter().any(|(x, _)| *x == t) && !self.refused.contains(&(s, t))
                {
                    eligible.entry(t).or_default().push(s);
                }
            }
        }
        let mut taken = BTreeSet::new();
        let mut batch = Vec::new();
        while batch.len() < requests {
            let best = eligible
                .iter()
                .map(|(t, ss)| {
                    (
                        ss.iter().filter(|s| !taken.contains(*s)).count(),
                        std::cmp::Reverse(*t),
                        *t,
                    )
                })
                .max();
            let Some((count, _, t)) = best else { break };
            if count == 0 {
                break;
            }
            let asked = eligible
                .remove(&t)
                .unwrap()
                .into_iter()
                .filter(|s| !taken.contains(s))
                .take(MAX_QUESTIONS)
                .collect::<Vec<_>>();
            taken.extend(asked.iter().copied());
            batch.push((t, asked));
        }
        batch
    }
}

/// What changes every answer at once: the run's dependencies and test/build
/// configuration. (The `execution` fingerprint also folds in the source, so it
/// moves with every edit and would re-ask everything.)
fn salt(run: &StoredRun) -> String {
    let f = &run.metadata.integrity.fingerprint;
    format!(
        "{:x}",
        Sha256::digest(format!("{}\n{}", f.dependencies, f.configuration))
    )
}

fn assess(root: &Path, run: &StoredRun, options: &Options) -> Result<Value, String> {
    if options.changed {
        // The checkout has changed since the run: its code is the snapshot
        // the run's assessment kept.
        let snapshot = coverage::load_snapshot(&run.directory.join(SNAPSHOT_FILE)).ok_or_else(|| {
            format!(
                "this run's sources were not kept; assess the run first (`{} runs <run> assertions assess`) before changing its code",
                crate::launcher_command()
            )
        })?;
        let stored = maps::load_manifest(run)?;
        let Some(language) = coverage::Language::from_manifest(&stored.manifest.language) else {
            return Err(format!(
                "unsupported language: {}",
                stored.manifest.language
            ));
        };
        let report = maps::coverage(run)?;
        let sources = snapshot
            .iter()
            .filter(|(file, _)| stored.manifest.files.contains_key(*file))
            .map(|(f, t)| (f.clone(), t.clone()))
            .collect();
        let population =
            coverage::population(root, &report, &sources, language)?.with_files(snapshot);
        return assess_changed(root, run, options, &population);
    }
    let inputs = maps::load_inputs(root, run)?;
    let Some(language) = coverage::Language::from_manifest(&inputs.inputs.language) else {
        return Err(format!(
            "Assertion coverage supports JavaScript, TypeScript, Python, Ruby, Go, Rust, Java and Kotlin runs; this run is {}",
            inputs.inputs.language
        ));
    };
    let report = maps::coverage(run)?;
    let population = coverage::population(root, &report, &inputs.inputs.files, language)?;
    let n = population.statements.len();
    let mut pass = Pass {
        population: &population,
        model: quality::model(),
        salt: salt(run),
        answers: vec![Vec::new(); n],
        orders: (0..n).map(|s| population.order(s, FIRST_TESTS)).collect(),
        wide: false,
        keys: BTreeMap::new(),
        refused: BTreeSet::new(),
    };
    let mut cache = Cache::load(root);
    let mut reused = pass.seed(&cache);
    let key = quality::setting("TYPESAFE_API_KEY");
    let progress = !options.json && std::io::IsTerminal::is_terminal(&std::io::stderr());
    let (mut requests, mut tokens, mut failures) = (0usize, 0u64, 0usize);
    if options.dry_run {
        let plan = pass.plan(usize::MAX);
        let bytes: usize = plan
            .iter()
            .map(|(t, asked)| {
                serde_json::to_vec(&population.request(*t, asked, pass.model))
                    .map(|b| quality::estimated_tokens(&b))
                    .unwrap_or(0)
            })
            .sum();
        let undecided = (0..n).filter(|&s| pass.undecided(s)).count();
        // The most the pass can ask: every undecided statement asked of as
        // many tests as it widens to, grouped by test as the rounds group
        // them. A fixed "1.5 to 3 times the first round" was 4x on
        // supergateway, where most statements need more than their first tests.
        let mut per_test = BTreeMap::<usize, usize>::new();
        for s in (0..n).filter(|&s| pass.undecided(s)) {
            for t in population.order(s, MORE_TESTS) {
                *per_test.entry(t).or_default() += 1;
            }
        }
        let most: usize = per_test.values().map(|c| c.div_ceil(MAX_QUESTIONS)).sum();
        let most = most.max(plan.len());
        let most_tokens = if plan.is_empty() {
            0
        } else {
            bytes * most / plan.len()
        };
        let cost = |tokens: usize| tokens as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS;
        return Ok(json!({
            "run": run.id, "dryRun": true, "statements": n, "answeredFromCache": n - undecided,
            "firstRound": {"requests": plan.len(), "inputTokens": bytes, "costUsd": cost(bytes)},
            "atMost": {"requests": most, "inputTokens": most_tokens, "costUsd": cost(most_tokens)},
            "note": "Statements their first tests are not judged to catch are asked of more tests, so the pass costs between the first round and the most.",
        }));
    }
    let endpoint = quality::endpoint()?;
    let agent = quality::client();
    // A round's requests are planned from the answers so far, so its size
    // decides how many questions early stopping saves; how many are in
    // flight decides only the wait. 40 is what eight workers planned; with
    // 24 in flight the three largest measured projects took half the time
    // for the same cost.
    let batch_size = ROUND_REQUESTS;
    loop {
        if (0..n).all(|s| !pass.undecided(s)) {
            if pass.wide {
                break;
            }
            pass.widen();
            reused += pass.seed(&cache);
            continue;
        }
        let key = key
            .as_deref()
            .ok_or("set TYPESAFE_API_KEY to ask Jev, or use --dry-run for an estimate")?;
        let planned = pass.plan(batch_size);
        if planned.is_empty() {
            // Undecided statements no test is left to ask about (the rest
            // were over Jev's limit): widen once, then stop.
            if pass.wide {
                break;
            }
            pass.widen();
            reused += pass.seed(&cache);
            continue;
        }
        let batch = within_limits(&population, &mut pass, planned);
        if batch.is_empty() {
            continue;
        }
        let bodies = batch
            .iter()
            .map(|(t, asked)| population.request(*t, asked, pass.model))
            .collect::<Vec<_>>();
        let results = send_all(&agent, endpoint, key, &bodies, options.workers);
        for ((t, asked), result) in batch.iter().zip(results) {
            requests += 1;
            let response = match result {
                Ok(r) => r,
                Err(e) => {
                    failures += 1;
                    if failures > MAX_FAILURES {
                        cache.save()?;
                        return Err(format!("stopped after {failures} failed requests: {e}"));
                    }
                    continue;
                }
            };
            failures = 0;
            tokens += response.usage.input_tokens;
            for (i, &s) in asked.iter().enumerate() {
                let p = (0..2)
                    .filter_map(|j| match response.answers.get(&format!("q{i}_{j}")) {
                        Some(quality::Answer::Noul { noul }) => Some(*noul),
                        _ => None,
                    })
                    .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))));
                let Some(p) = p else { continue };
                pass.answers[s].push((*t, p));
                let k = pass.key(s, *t);
                cache.answers.insert(k, p);
            }
        }
        cache.save()?;
        if progress {
            let left = (0..n).filter(|&s| pass.undecided(s)).count();
            eprintln!(
                "[supercov] assertions: {requests} requests, {left} statements still being asked"
            );
        }
    }
    let result = summary(run, &population, &pass, requests, tokens, reused);
    let _ = std::fs::write(
        run.directory.join(RESULT_FILE),
        serde_json::to_vec(&result).unwrap_or_default(),
    );
    // Every file the questions rendered, and the run's sources, as verified.
    let mut kept = population.files_read();
    kept.extend(
        inputs
            .inputs
            .files
            .iter()
            .map(|(f, t)| (f.clone(), t.clone())),
    );
    let _ = coverage::save_snapshot(&run.directory.join(SNAPSHOT_FILE), &kept);
    view(result, &Target::Overview, 0, Some(PAGE))
}

/// `assess --changed`: every test that ran a statement in changed code is
/// asked about it, with no early stop, so each affected test has its own
/// answer. The questions are the whole-app pass's own, so their answers are
/// shared through the cache both ways.
fn assess_changed(
    root: &Path,
    run: &StoredRun,
    options: &Options,
    population: &Population,
) -> Result<Value, String> {
    let affected = maps::affected_tests(root, run)?;
    let mut current = crate::tests_query::CurrentLines::new(root);
    let mut changed = BTreeSet::new();
    let mut seen_code = BTreeSet::new();
    for test in affected["affected"].as_array().into_iter().flatten() {
        for code in test["changedCode"].as_array().into_iter().flatten() {
            if !seen_code.insert(code.to_string()) {
                continue;
            }
            let file = code["file"].as_str().unwrap_or("");
            let statements = population
                .statements
                .iter()
                .enumerate()
                .filter(|(_, st)| st.file == file)
                .map(|(i, st)| json!({"index": i, "line": st.line, "text": st.text.lines().next().unwrap_or("").trim()}))
                .collect::<Vec<_>>();
            for statement in crate::tests_query::in_changed_code(&mut current, code, statements) {
                if let Some(i) = statement["index"].as_u64() {
                    changed.insert(i as usize);
                }
            }
        }
    }
    let model = quality::model();
    let salt = salt(run);
    let mut cache = Cache::load(root);
    let mut answers = BTreeMap::<usize, Vec<(usize, f64)>>::new();
    let mut wanted = BTreeMap::<usize, Vec<usize>>::new(); // test -> statements
    let mut keys = BTreeMap::<(usize, usize), String>::new();
    let mut reused = 0;
    for &s in &changed {
        for t in population.order(s, IMPACT_TESTS) {
            let key = population.pair_key(s, t, model, &salt);
            match cache.answers.get(&key) {
                Some(&p) => {
                    answers.entry(s).or_default().push((t, p));
                    reused += 1;
                }
                None => wanted.entry(t).or_default().push(s),
            }
            keys.insert((s, t), key);
        }
    }
    let plan = wanted
        .into_iter()
        .flat_map(|(t, ss)| {
            ss.chunks(MAX_QUESTIONS)
                .map(|c| (t, c.to_vec()))
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>();
    let bodies = plan
        .iter()
        .map(|(t, asked)| population.request(*t, asked, model))
        .collect::<Vec<_>>();
    if options.dry_run {
        let tokens: usize = bodies
            .iter()
            .map(|b| {
                serde_json::to_vec(b)
                    .map(|b| quality::estimated_tokens(&b))
                    .unwrap_or(0)
            })
            .sum();
        return Ok(json!({
            "run": run.id, "dryRun": true, "changed": changed.len(), "answeredFromCache": reused,
            "firstRound": {"requests": plan.len(), "inputTokens": tokens, "costUsd": tokens as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS},
        }));
    }
    let (mut tokens, mut failed) = (0u64, 0usize);
    if !plan.is_empty() {
        let key = quality::setting("TYPESAFE_API_KEY")
            .ok_or("set TYPESAFE_API_KEY to ask Jev, or use --dry-run for an estimate")?;
        let endpoint = quality::endpoint()?;
        let agent = quality::client();
        let results = send_all(&agent, endpoint, &key, &bodies, options.workers);
        for ((t, asked), result) in plan.iter().zip(results) {
            let Ok(response) = result else {
                failed += 1;
                continue;
            };
            tokens += response.usage.input_tokens;
            for (i, &s) in asked.iter().enumerate() {
                let p = (0..2)
                    .filter_map(|j| match response.answers.get(&format!("q{i}_{j}")) {
                        Some(quality::Answer::Noul { noul }) => Some(*noul),
                        _ => None,
                    })
                    .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))));
                let Some(p) = p else { continue };
                answers.entry(s).or_default().push((*t, p));
                if let Some(k) = keys.get(&(s, *t)) {
                    cache.answers.insert(k.clone(), p);
                }
            }
        }
        cache.save()?;
    }
    let statements = changed
        .iter()
        .map(|&s| {
            let st = &population.statements[s];
            let got = answers.get(&s).map_or(0, Vec::len);
            json!({
                "file": st.file, "line": st.line, "text": st.text.lines().next().unwrap_or("").trim(),
                // Every test that ran it answered: one without an answer did
                // not run it.
                "complete": st.tests.len() <= IMPACT_TESTS && got >= st.tests.len(),
                "answers": answers.get(&s).into_iter().flatten().map(|(t, p)| json!({
                    "file": population.tests[*t].file, "name": population.tests[*t].name, "p": p,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    // A statement more than IMPACT_TESTS tests ran is asked of that many.
    let capped = changed
        .iter()
        .filter(|&&s| population.statements[s].tests.len() > IMPACT_TESTS)
        .count();
    let result = json!({
        "run": run.id, "model": model, "changed": changed.len(), "capped": capped,
        "requests": plan.len(), "failedRequests": failed, "answersReused": reused,
        "inputTokens": tokens, "costUsd": tokens as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS,
        "statements": statements,
    });
    let _ = std::fs::write(
        run.directory.join(IMPACT_FILE),
        serde_json::to_vec(&result).unwrap_or_default(),
    );
    Ok(json!({"run": run.id, "impact": true, "summary": result}))
}

/// `assess --changed`'s saved answers for a run, if any.
pub(crate) fn impact(run: &StoredRun) -> Option<Value> {
    std::fs::read(run.directory.join(IMPACT_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

fn send_all(
    agent: &ureq::Agent,
    endpoint: &str,
    key: &str,
    bodies: &[Value],
    workers: usize,
) -> Vec<Result<quality::ApiResponse, String>> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let next = AtomicUsize::new(0);
    let out = std::sync::Mutex::new((0..bodies.len()).map(|_| None).collect::<Vec<_>>());
    std::thread::scope(|scope| {
        for _ in 0..workers.min(bodies.len()) {
            scope.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(body) = bodies.get(i) else { return };
                    let r = quality::evaluate(agent, endpoint, key, body);
                    out.lock().expect("results are only locked to store one")[i] = Some(r);
                }
            });
        }
    });
    out.into_inner()
        .expect("results are only locked to store one")
        .into_iter()
        .map(|r| r.unwrap_or_else(|| Err("not sent".into())))
        .collect()
}

fn change_name(change: Change, language: coverage::Language) -> &'static str {
    use coverage::Language::*;
    match (change, language) {
        (Change::ReturnUndefined, Python) => "returns None",
        (Change::ValueUndefined, Python) => "value becomes None",
        (Change::ReturnUndefined, Ruby) => "returns nil",
        (Change::ValueUndefined, Ruby) => "value becomes nil",
        (Change::ReturnUndefined, Go) => "returns zero values",
        (Change::ValueUndefined, Go) => "value becomes the zero value",
        (Change::ReturnUndefined, Rust) => "returns a different value",
        (Change::ValueUndefined, Rust) => "value replaced",
        (Change::ReturnUndefined, Jvm) => "returns the default",
        (Change::ValueUndefined, Jvm) => "value becomes the default",
        (change, _) => change_name_js(change),
    }
}

fn change_name_js(change: Change) -> &'static str {
    match change {
        Change::Invert => "condition inverted",
        Change::ReturnUndefined => "returns undefined",
        Change::ValueUndefined => "value becomes undefined",
        Change::ExpressionUndefined => "value becomes undefined",
        Change::Skip => "skipped",
        Change::Flip => "boolean flipped",
        Change::BareError => "throws a bare Error",
    }
}

fn summary(
    run: &StoredRun,
    population: &Population,
    pass: &Pass,
    requests: usize,
    tokens: u64,
    reused: usize,
) -> Value {
    let statements = population
        .statements
        .iter()
        .enumerate()
        .map(|(s, st)| {
            let best = pass.answers[s].iter().cloned().fold(None, |m: Option<(usize, f64)>, a| match m { Some(m) if m.1 >= a.1 => Some(m), _ => Some(a) });
            json!({
                "file": st.file, "line": st.line, "text": st.text.lines().next().unwrap_or("").trim(),
                "change": change_name(st.change, population.language), "replacement": st.replacement,
                "asserted": pass.asserted(s),
                "answer": best.map(|b| b.1),
                "test": best.map(|(t, _)| json!({"file": population.tests[t].file, "name": population.tests[t].name})),
                "testsAsked": pass.answers[s].len(), "testsRunningIt": st.tests.len(),
                // Each asked test's own answer: what `tests affected` reads
                // to say which tests check a changed statement.
                "answers": pass.answers[s].iter().map(|(t, p)| json!({
                    "file": population.tests[*t].file, "name": population.tests[*t].name, "p": p,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>();
    let asserted = statements.iter().filter(|s| s["asserted"] == true).count();
    let total = statements.len();
    json!({
        "run": run.id, "model": pass.model,
        "summary": {
            "statements": total, "asserted": asserted, "notAsserted": total - asserted,
            "percentage": if total == 0 { Value::Null } else { json!((asserted as f64 * 1000.0 / total as f64).round() / 10.0) },
            "requests": requests, "answersReused": reused, "inputTokens": tokens, "pairsOverLimit": pass.refused.len(),
            "costUsd": tokens as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS,
        },
        "basis": "Jev, from what each test ran; a statement is asserted when some test that runs it is judged to fail if it changes",
        "statements": statements,
    })
}

/// The run's saved result, when it has been assessed.
pub(crate) fn saved(run: &StoredRun) -> Option<Value> {
    std::fs::read(run.directory.join(RESULT_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

/// The lines of the statements judged asserted in a run, by file.
pub(crate) fn asserted_lines(run: &StoredRun) -> BTreeMap<String, BTreeSet<u64>> {
    let mut out = BTreeMap::<String, BTreeSet<u64>>::new();
    for s in saved(run)
        .and_then(|r| r["statements"].as_array().cloned())
        .unwrap_or_default()
    {
        if s["asserted"] == true
            && let (Some(file), Some(line)) = (s["file"].as_str(), s["line"].as_u64())
        {
            out.entry(file.to_owned()).or_default().insert(line);
        }
    }
    out
}

/// What the run's saved assessment says about one file, for the coverage
/// file view: its share asserted and the lines of the statements not asserted.
pub(crate) fn for_file(run: &StoredRun, file: &str) -> Value {
    let Some(result) = saved(run) else {
        return not_assessed(run);
    };
    let here = result["statements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["file"] == file)
        .collect::<Vec<_>>();
    let asserted = here.iter().filter(|s| s["asserted"] == true).count() as u64;
    let mut lines = here
        .iter()
        .filter(|s| s["asserted"] == false)
        .filter_map(|s| s["line"].as_u64())
        .collect::<Vec<_>>();
    lines.sort_unstable();
    lines.dedup();
    json!({
        "available": true,
        "statements": here.len(),
        "asserted": asserted,
        "notAsserted": here.len() as u64 - asserted,
        "percentage": percentage(asserted, here.len() as u64),
        "notAssertedLines": lines,
        "inspect": format!("{} runs {} assertions {}", crate::launcher_command(), quote(&run.id), quote(file)),
    })
}

/// What the run's saved assessment says about the statements on one line, for
/// the coverage line view.
pub(crate) fn for_line(run: &StoredRun, file: &str, line: usize) -> Value {
    let Some(result) = saved(run) else {
        return not_assessed(run);
    };
    let here = result["statements"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["file"] == file && s["line"].as_u64() == Some(line as u64))
        .map(|s| {
            json!({
                "text": s["text"],
                "change": s["change"],
                "asserted": s["asserted"],
                "test": s["test"],
                "answer": s["answer"],
            })
        })
        .collect::<Vec<_>>();
    json!({
        "available": true,
        "statements": here,
        "inspect": format!("{} runs {} assertions {}", crate::launcher_command(), quote(&run.id), quote(&format!("{file}:{line}"))),
    })
}

fn not_assessed(run: &StoredRun) -> Value {
    json!({"available": false, "assess": format!("{} runs {} assertions assess", crate::launcher_command(), quote(&run.id))})
}

/// The run's saved result, or where the last one is when this run has none.
fn read(run: &StoredRun, runs: &[StoredRun], options: &Options) -> Result<Value, String> {
    if let Some(result) = saved(run) {
        return view(result, &options.target, options.offset, options.limit);
    }
    let last = runs
        .iter()
        .filter(|r| r.id != run.id)
        .filter_map(|r| saved(r).map(|v| (r, v)))
        .max_by(|(a, _), (b, _)| a.metadata.started_at.cmp(&b.metadata.started_at))
        .map(|(r, v)| json!({"run": r.id, "percentage": v["summary"]["percentage"]}));
    Ok(json!({
        "run": run.id,
        "assessed": false,
        "lastAssessed": last,
        "assess": format!("{} runs {} assertions assess", crate::launcher_command(), run.id),
        "note": "Answers from earlier assessments are reused when the same question comes up, so assessing a later run usually asks only about what changed; `assess --dry-run` shows how many questions it would send.",
    }))
}

fn page(offset: usize, limit: Option<usize>, returned: usize, total: usize) -> Value {
    json!({
        "offset": offset,
        "limit": limit,
        "returned": returned,
        "total": total,
        "nextOffset": (offset + returned < total).then_some(offset + returned),
    })
}

fn window(items: Vec<Value>, offset: usize, limit: Option<usize>) -> Vec<Value> {
    items
        .into_iter()
        .skip(offset)
        .take(limit.unwrap_or(usize::MAX))
        .collect()
}

fn percentage(asserted: u64, total: u64) -> Value {
    if total == 0 {
        Value::Null
    } else {
        json!((asserted as f64 * 1000.0 / total as f64).round() / 10.0)
    }
}

/// A statement as a listing shows it. Each test's answer stays in
/// assertion-coverage.json, and the statement view shows them: square/moshi's
/// 222 statements with up to 15 answers each came to 94 KB.
fn slim(statement: &Value) -> Value {
    let mut statement = statement.clone();
    if let Some(object) = statement.as_object_mut() {
        object.remove("answers");
    }
    statement
}

/// One reading of the saved result: the overview by file, one file, one
/// statement or one test, a page at a time so no answer has to be cut short.
fn view(
    mut result: Value,
    target: &Target,
    offset: usize,
    limit: Option<usize>,
) -> Result<Value, String> {
    let all = result["statements"]
        .take()
        .as_array()
        .cloned()
        .unwrap_or_default();
    let run = result["run"].as_str().unwrap_or("").to_owned();
    let data = result
        .as_object_mut()
        .ok_or("the saved assessment is not a JSON object")?;
    data.remove("statements");
    match target {
        Target::Overview => {
            let mut files = BTreeMap::<String, (u64, u64)>::new();
            for statement in &all {
                let entry = files
                    .entry(statement["file"].as_str().unwrap_or("").to_owned())
                    .or_default();
                entry.0 += 1;
                entry.1 += u64::from(statement["asserted"] == true);
            }
            let mut rows = files
                .into_iter()
                .map(|(file, (total, asserted))| {
                    json!({
                        "file": file,
                        "statements": total,
                        "asserted": asserted,
                        "notAsserted": total - asserted,
                        "percentage": percentage(asserted, total),
                    })
                })
                .collect::<Vec<_>>();
            rows.sort_by(|a, b| {
                b["notAsserted"]
                    .as_u64()
                    .cmp(&a["notAsserted"].as_u64())
                    .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            });
            let total = rows.len();
            let shown = window(rows, offset, limit);
            data.insert("view".into(), json!("files"));
            data.insert("page".into(), page(offset, limit, shown.len(), total));
            data.insert("files".into(), json!(shown));
        }
        Target::File(file) => {
            let here = all
                .iter()
                .filter(|statement| statement["file"] == file.as_str())
                .collect::<Vec<_>>();
            if here.is_empty() {
                return Err(format!(
                    "no assessed statement in {file}; `{} runs {run} assertions` lists the assessed files",
                    crate::launcher_command()
                ));
            }
            let asserted = here.iter().filter(|s| s["asserted"] == true).count() as u64;
            let mut not = here
                .iter()
                .filter(|s| s["asserted"] == false)
                .map(|s| slim(s))
                .collect::<Vec<_>>();
            not.sort_by_key(|s| s["line"].as_u64());
            let total = not.len();
            let shown = window(not, offset, limit);
            data.insert("view".into(), json!("file"));
            data.insert("file".into(), json!(file));
            data.insert(
                "fileSummary".into(),
                json!({
                    "statements": here.len(),
                    "asserted": asserted,
                    "notAsserted": here.len() as u64 - asserted,
                    "percentage": percentage(asserted, here.len() as u64),
                }),
            );
            data.insert("page".into(), page(offset, limit, shown.len(), total));
            data.insert("notAsserted".into(), json!(shown));
        }
        Target::Line(file, line) => {
            let here = all
                .iter()
                .filter(|s| s["file"] == file.as_str() && s["line"].as_u64() == Some(*line))
                .map(|statement| {
                    let mut statement = statement.clone();
                    let mut answers = statement["answers"].as_array().cloned().unwrap_or_default();
                    answers.sort_by(|a, b| {
                        b["p"]
                            .as_f64()
                            .unwrap_or(0.0)
                            .total_cmp(&a["p"].as_f64().unwrap_or(0.0))
                    });
                    for answer in &mut answers {
                        let catches = answer["p"].as_f64().unwrap_or(0.0) >= THRESHOLD;
                        answer["catches"] = json!(catches);
                    }
                    statement["answers"] = json!(answers);
                    statement
                })
                .collect::<Vec<_>>();
            if here.is_empty() {
                return Err(format!(
                    "no assessed statement at {file}:{line}; `{} runs {run} assertions {}` lists the file's statements that are not asserted",
                    crate::launcher_command(),
                    quote(file)
                ));
            }
            data.insert("view".into(), json!("statement"));
            data.insert("file".into(), json!(file));
            data.insert("line".into(), json!(line));
            data.insert("statements".into(), json!(here));
        }
        Target::Test(query) => {
            let named = |exact: bool| {
                all.iter()
                    .flat_map(|s| s["answers"].as_array().cloned().unwrap_or_default())
                    .filter_map(|a| {
                        let name = a["name"].as_str()?.to_owned();
                        let matches = if exact {
                            name == *query
                        } else {
                            name.contains(query.as_str())
                        };
                        matches.then(|| (a["file"].as_str().unwrap_or("").to_owned(), name))
                    })
                    .collect::<BTreeSet<_>>()
            };
            let mut tests = named(true);
            if tests.is_empty() {
                tests = named(false);
            }
            let (test_file, test_name) = match tests.len() {
                0 => {
                    return Err(format!(
                        "no assessed test is named {query}; `{} runs {run} tests` lists the run's tests",
                        crate::launcher_command()
                    ));
                }
                1 => tests.into_iter().next().unwrap(),
                n => {
                    let names = tests
                        .iter()
                        .take(10)
                        .map(|(file, name)| format!("  {name} ({file})"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    return Err(format!(
                        "{n} assessed tests match {query}; give one's full name:\n{names}"
                    ));
                }
            };
            let mut items = all
                .iter()
                .filter_map(|s| {
                    let answer = s["answers"].as_array()?.iter().find(|a| {
                        a["name"] == test_name.as_str() && a["file"] == test_file.as_str()
                    })?;
                    let p = answer["p"].as_f64().unwrap_or(0.0);
                    Some(json!({
                        "file": s["file"],
                        "line": s["line"],
                        "text": s["text"],
                        "change": s["change"],
                        "p": p,
                        "catches": p >= THRESHOLD,
                    }))
                })
                .collect::<Vec<_>>();
            // What it catches first, then the rest, each in source order.
            items.sort_by(|a, b| {
                (b["catches"] == true)
                    .cmp(&(a["catches"] == true))
                    .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
                    .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
            });
            let catches = items.iter().filter(|i| i["catches"] == true).count();
            let total = items.len();
            let shown = window(items, offset, limit);
            data.insert("view".into(), json!("test"));
            data.insert("test".into(), json!({"file": test_file, "name": test_name}));
            data.insert(
                "testSummary".into(),
                json!({"asked": total, "catches": catches}),
            );
            data.insert("page".into(), page(offset, limit, shown.len(), total));
            data.insert("statements".into(), json!(shown));
        }
    }
    Ok(result)
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// The JSON envelope for `data`, listing fewer statements when the full
/// listing would not fit in one response, and saying so.
fn fitted_json(mut data: Value) -> Result<String, agent_json::ResponseTooLarge> {
    loop {
        match agent_json::success("runs.assertions", &data, None) {
            Ok(output) => return Ok(output),
            Err(size) => {
                // Only an --all listing can come to this: halve its page and
                // say where the next one starts.
                let Some(key) = ["files", "notAsserted", "statements"]
                    .into_iter()
                    .find(|key| data[*key].as_array().is_some_and(|list| list.len() > 1))
                else {
                    return Err(size);
                };
                let listed = data[key].as_array().map_or(0, Vec::len);
                let keep = listed / 2;
                data[key] = json!(data[key].as_array().unwrap()[..keep]);
                if let Some(page) = data["page"].as_object_mut() {
                    let offset = page["offset"].as_u64().unwrap_or(0);
                    page.insert("returned".into(), json!(keep));
                    page.insert("nextOffset".into(), json!(offset + keep as u64));
                }
                data["truncated"] = json!(
                    "the listing was shortened to fit one response; page.nextOffset is where the rest starts"
                );
            }
        }
    }
}

fn render(data: &Value) -> String {
    if data["impact"] == true {
        let s = &data["summary"];
        let capped = s["capped"].as_u64().unwrap_or(0);
        let asked = if capped == 0 {
            "asked of every test that ran them".to_owned()
        } else {
            format!(
                "asked of the tests that ran them, {IMPACT_TESTS} at most each ({capped} ran under more)"
            )
        };
        return format!(
            "Run {}: {} changed statement(s) {asked}: {}, ${:.4}; {} reused.\n`{} runs {} tests affected` now says which affected tests catch the change.\n",
            data["run"].as_str().unwrap_or(""),
            s["changed"],
            counted(&s["requests"], "request", "requests"),
            s["costUsd"].as_f64().unwrap_or(0.0),
            counted(&s["answersReused"], "answer", "answers"),
            crate::launcher_command(),
            data["run"].as_str().unwrap_or(""),
        );
    }
    if data["dryRun"] == true && data.get("changed").is_some() {
        return format!(
            "Run {}: {} changed statement(s), {} answers in the cache. {} requests, about {} input tokens, ${:.4}.\n",
            data["run"].as_str().unwrap_or(""),
            data["changed"],
            data["answeredFromCache"],
            data["firstRound"]["requests"],
            data["firstRound"]["inputTokens"],
            data["firstRound"]["costUsd"].as_f64().unwrap_or(0.0),
        );
    }
    if data["assessed"] == false {
        let mut out = format!(
            "Run {}: assertions not assessed for this run.\n",
            data["run"].as_str().unwrap_or("")
        );
        if let Some(last) = data["lastAssessed"].as_object() {
            out.push_str(&format!(
                "Last assessed: {}, {}% asserted.\n",
                last["run"].as_str().unwrap_or(""),
                last["percentage"]
            ));
        }
        out.push_str(&format!(
            "Assess it with `{}` (`--dry-run` estimates the cost first).\n{}\n",
            data["assess"].as_str().unwrap_or(""),
            data["note"].as_str().unwrap_or("")
        ));
        return out;
    }
    if data["dryRun"] == true {
        let f = &data["firstRound"];
        return format!(
            "Run {}: {} executed statements, {} answered from the cache.\nFirst round: {} requests, about {} input tokens, ${:.4}.\nAt most: {} requests, about {} input tokens, ${:.4}.\n{}\n",
            data["run"].as_str().unwrap_or(""),
            data["statements"],
            data["answeredFromCache"],
            f["requests"],
            f["inputTokens"],
            f["costUsd"].as_f64().unwrap_or(0.0),
            data["atMost"]["requests"],
            data["atMost"]["inputTokens"],
            data["atMost"]["costUsd"].as_f64().unwrap_or(0.0),
            data["note"].as_str().unwrap_or("")
        );
    }
    let run = data["run"].as_str().unwrap_or("");
    let mut out = match data["view"].as_str() {
        Some("file") => render_file(run, data),
        Some("statement") => render_statement(run, data),
        Some("test") => render_test(run, data),
        _ => render_files(run, data),
    };
    if data["view"] == "files" {
        let s = &data["summary"];
        out.push_str(&format!(
            "\nAssessed by Jev: {}, {} input tokens, ${:.4}; {} reused from .supercov/assertions/.\n",
            counted(&s["requests"], "request", "requests"),
            s["inputTokens"],
            s["costUsd"].as_f64().unwrap_or(0.0),
            counted(&s["answersReused"], "answer", "answers")
        ));
    }
    out
}

/// `1 answer`, `2 answers`: a count with its noun.
fn counted(value: &Value, one: &str, many: &str) -> String {
    let n = value.as_u64().unwrap_or(0);
    format!("{n} {}", if n == 1 { one } else { many })
}

fn share(value: &Value) -> String {
    value
        .as_f64()
        .map(|p| format!("{p}%"))
        .unwrap_or_else(|| "nothing".into())
}

/// `showing a-b of n`, and the command for the next page when there is one.
fn page_footer(data: &Value, command: &str) -> String {
    let page = &data["page"];
    let offset = page["offset"].as_u64().unwrap_or(0);
    let returned = page["returned"].as_u64().unwrap_or(0);
    let total = page["total"].as_u64().unwrap_or(0);
    let start = if returned == 0 { 0 } else { offset + 1 };
    let mut out = format!("showing {start}-{} of {total}\n", offset + returned);
    if let Some(next) = page["nextOffset"].as_u64() {
        let limit = match page["limit"].as_u64() {
            Some(limit) if limit as usize != PAGE => format!(" --limit {limit}"),
            _ => String::new(),
        };
        out.push_str(&format!("next page: {command} --offset {next}{limit}\n"));
    }
    out
}

fn render_files(run: &str, data: &Value) -> String {
    let s = &data["summary"];
    let mut out = format!(
        "Run {run}: {} asserted ({} of {} executed statements)\n",
        share(&s["percentage"]),
        s["asserted"],
        s["statements"]
    );
    let files = data["files"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        return out;
    }
    out.push_str(
        "\nFiles, most statements not asserted first: a statement is not asserted when none of the tests asked about it was judged to fail if it changed. Up to {MORE_TESTS} of the tests that run a statement are asked, so a test never asked can still catch it.\n NOT ASSERTED  ASSERTED  FILE\n",
    );
    for file in &files {
        out.push_str(&format!(
            " {:>12}  {:>8}  {}\n",
            file["notAsserted"].as_u64().unwrap_or(0),
            format!("{}/{}", file["asserted"], file["statements"]),
            file["file"].as_str().unwrap_or("")
        ));
    }
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions",
            crate::launcher_command(),
            quote(run)
        ),
    ));
    if let Some(first) = files.iter().find(|f| f["notAsserted"].as_u64() > Some(0)) {
        out.push_str(&format!(
            "inspect a file: {} runs {} assertions {}\n",
            crate::launcher_command(),
            quote(run),
            quote(first["file"].as_str().unwrap_or(""))
        ));
    }
    out
}

fn render_file(run: &str, data: &Value) -> String {
    let file = data["file"].as_str().unwrap_or("");
    let s = &data["fileSummary"];
    let mut out = format!(
        "Run {run}: {file}: {} asserted ({} of {} statements)\n",
        share(&s["percentage"]),
        s["asserted"],
        s["statements"]
    );
    let not = data["notAsserted"].as_array().cloned().unwrap_or_default();
    if s["notAsserted"].as_u64() == Some(0) {
        out.push_str("Every statement this file ran is asserted.\n");
        return out;
    }
    out.push_str(&format!("\nNot asserted: none of the tests asked was judged to fail if they changed. Up to {MORE_TESTS} of the tests that run a statement are asked; one never asked can still catch it.\n  LINE  STATEMENT  (change)  tests asked  closest test\n"));
    for st in &not {
        let closest = st["test"]["name"]
            .as_str()
            .map(|name| {
                format!(
                    "  {name} {}",
                    st["answer"]
                        .as_f64()
                        .map(|p| format!("{:.0}%", p * 100.0))
                        .unwrap_or_default()
                )
            })
            .unwrap_or_default();
        let asked = match (st["testsAsked"].as_u64(), st["testsRunningIt"].as_u64()) {
            (Some(asked), Some(ran)) => format!("  {asked} of {ran}"),
            _ => String::new(),
        };
        out.push_str(&format!(
            " {:>5}  {}  ({}){asked}{closest}\n",
            st["line"].as_u64().unwrap_or(0),
            st["text"].as_str().unwrap_or(""),
            st["change"].as_str().unwrap_or("")
        ));
    }
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions {}",
            crate::launcher_command(),
            quote(run),
            quote(file)
        ),
    ));
    if let Some(first) = not.first() {
        out.push_str(&format!(
            "inspect a statement: {} runs {} assertions {}\n",
            crate::launcher_command(),
            quote(run),
            quote(&format!("{file}:{}", first["line"]))
        ));
    }
    out
}

fn render_statement(run: &str, data: &Value) -> String {
    let mut out = format!(
        "Run {run}: {}:{}\n",
        data["file"].as_str().unwrap_or(""),
        data["line"]
    );
    for st in data["statements"].as_array().cloned().unwrap_or_default() {
        out.push_str(&format!(
            "\n  {}\n  Change: {}{}\n  {}: {} test(s) run it, {} asked{}\n",
            st["text"].as_str().unwrap_or(""),
            st["change"].as_str().unwrap_or(""),
            st["replacement"]
                .as_str()
                .map(|r| format!(" -> `{r}`"))
                .unwrap_or_default(),
            if st["asserted"] == true {
                "Asserted"
            } else {
                "Not asserted"
            },
            st["testsRunningIt"],
            st["testsAsked"],
            // Not asserted by the asked tests is not the same as by every test.
            if st["asserted"] != true && st["testsAsked"].as_u64() < st["testsRunningIt"].as_u64() {
                "; the tests not asked were not judged"
            } else {
                ""
            }
        ));
        for answer in st["answers"].as_array().cloned().unwrap_or_default() {
            out.push_str(&format!(
                "    {:>4}  {}  {} ({})\n",
                format!("{:.0}%", answer["p"].as_f64().unwrap_or(0.0) * 100.0),
                if answer["catches"] == true {
                    "would fail "
                } else {
                    "would pass "
                },
                answer["name"].as_str().unwrap_or(""),
                answer["file"].as_str().unwrap_or("")
            ));
        }
    }
    out.push_str(
        "\nThe percentage is how likely Jev judged the test to fail with the change made.\n",
    );
    out
}

fn render_test(run: &str, data: &Value) -> String {
    let test = &data["test"];
    let name = test["name"].as_str().unwrap_or("");
    let s = &data["testSummary"];
    let mut out = format!(
        "Run {run}: {name} ({}): judged to catch a change to {} of the {} statements it was asked about\n",
        test["file"].as_str().unwrap_or(""),
        s["catches"],
        s["asked"]
    );
    let mut heading = "";
    for item in data["statements"].as_array().cloned().unwrap_or_default() {
        let now = if item["catches"] == true {
            "Catches"
        } else {
            "Would not catch"
        };
        if now != heading {
            out.push_str(&format!("\n{now}\n"));
            heading = now;
        }
        out.push_str(&format!(
            "  {}:{}  {}  ({})  {:.0}%\n",
            item["file"].as_str().unwrap_or(""),
            item["line"],
            item["text"].as_str().unwrap_or(""),
            item["change"].as_str().unwrap_or(""),
            item["p"].as_f64().unwrap_or(0.0) * 100.0
        ));
    }
    out.push('\n');
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions --test {}",
            crate::launcher_command(),
            quote(run),
            quote(name)
        ),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// square/moshi: 222 statements not asserted, each with up to 15 tests'
    /// answers, came to 94 KB and the listing failed outright. A file's page
    /// leaves the answers out, and even every statement at once fits.
    #[test]
    fn a_long_listing_is_slimmed_and_shortened_to_fit_one_response() {
        let answers = (0..15)
            .map(|i| json!({"file": "src/test/AVeryLongTestFileName.java", "name": format!("com.example.SomeTest#aDescriptiveTestName{i}[Utf8]"), "p": 0.3}))
            .collect::<Vec<_>>();
        let statements = (0..4000)
            .map(|line| json!({"file": "src/main/Reader.kt", "line": line, "text": "x".repeat(150), "asserted": false, "answer": 0.3, "answers": answers}))
            .collect::<Vec<_>>();
        let result = json!({"run": "run_x", "summary": {}, "statements": statements});
        let file = Target::File("src/main/Reader.kt".into());
        let paged = view(result.clone(), &file, 0, Some(PAGE)).unwrap();
        assert!(paged["notAsserted"][0].get("answers").is_none());
        assert_eq!(paged["page"]["nextOffset"], 20);
        assert!(fitted_json(paged).is_ok());
        let everything = view(result, &file, 0, None).unwrap();
        let output = fitted_json(everything).expect("shortened to fit");
        let parsed: Value = serde_json::from_str(&output).unwrap();
        let returned = parsed["data"]["page"]["returned"].as_u64().unwrap();
        assert!(returned < 4000);
        assert_eq!(parsed["data"]["page"]["nextOffset"], returned);
        assert!(parsed["data"]["truncated"].is_string());
    }

    #[test]
    fn a_reading_goes_from_files_to_a_statement_and_to_a_test() {
        let answer = |name: &str, p: f64| json!({"file": "t.js", "name": name, "p": p});
        let statement = |file: &str, line: u64, asserted: bool, answers: Vec<Value>| json!({"file": file, "line": line, "text": "x;", "change": "skipped", "asserted": asserted, "answers": answers});
        let result = json!({"run": "run_x", "summary": {}, "statements": [
            statement("a.js", 1, true, vec![answer("one", 0.9), answer("two", 0.1)]),
            statement("a.js", 2, false, vec![answer("two", 0.2)]),
            statement("b.js", 1, false, vec![answer("one", 0.3)]),
            statement("b.js", 2, false, vec![answer("one", 0.4)]),
        ]});
        let files = view(result.clone(), &Target::Overview, 0, Some(PAGE)).unwrap();
        assert_eq!(files["files"][0]["file"], "b.js");
        assert_eq!(files["files"][1]["notAsserted"], 1);
        let line = view(result.clone(), &Target::Line("a.js".into(), 1), 0, None).unwrap();
        assert_eq!(line["statements"][0]["answers"][0]["catches"], true);
        assert_eq!(line["statements"][0]["answers"][1]["catches"], false);
        let test = view(result.clone(), &Target::Test("one".into()), 0, Some(PAGE)).unwrap();
        assert_eq!(test["testSummary"]["catches"], 1);
        assert_eq!(test["statements"][0]["catches"], true);
        assert!(view(result.clone(), &Target::File("c.js".into()), 0, None).is_err());
        assert!(view(result, &Target::Test("o".into()), 0, None).is_err());
    }
}
