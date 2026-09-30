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
    assertion_store as maps,
    run_store::{StoredRun, discover_runs, select_run},
};

const HELP: &str = r#"Usage: supercov runs <run> assertions [--all | --limit N] [--json]
       supercov runs <run> assertions assess [--dry-run] [--workers N] [--all | --limit N] [--json]

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
questions not yet answered. JavaScript and TypeScript runs.

--all         List every statement that is not asserted (default: 50).
--limit N     List N statements that are not asserted.
--dry-run     (assess) Estimate the requests and cost; send nothing.
--workers N   (assess) Requests in flight at once (default 8). More is faster
              and costs slightly more, since fewer questions stop early.
--json        Structured output for integrations.
"#;

const CACHE_VERSION: u32 = 1;
/// Consecutive failed requests after which the pass stops rather than keep
/// spending: an exhausted account or an outage fails every request alike.
const MAX_FAILURES: usize = 20;

/// The file in a run's directory holding its assessed result.
pub const RESULT_FILE: &str = "assertion-coverage.json";

struct Options {
    assess: bool,
    dry_run: bool,
    json: bool,
    limit: Option<usize>,
    workers: usize,
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
            read(run, &inventory.runs, options.limit)
        }
    })();
    match result {
        Ok(data) => {
            if options.json {
                match agent_json::success("runs.assertions", &data, None) {
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
        dry_run: false,
        json: false,
        limit: Some(50),
        workers: 8,
    };
    let mut i = 0;
    if args.first().is_some_and(|a| a == "assess") {
        o.assess = true;
        i = 1;
    }
    while i < args.len() {
        match args[i].as_str() {
            "--dry-run" | "--workers" if !o.assess => {
                return Err(format!("{} belongs to `assertions assess`", args[i]));
            }
            "--dry-run" => o.dry_run = true,
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
        !self.asserted(statement) && self.answers[statement].len() < self.limit(statement)
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
                if !self.answers[s].iter().any(|(x, _)| *x == t) {
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
    let inputs = maps::load_inputs(root, run)?;
    if inputs.inputs.language != "javascript" {
        return Err(format!(
            "Assertions coverage supports JavaScript and TypeScript runs so far; this run is {}",
            inputs.inputs.language
        ));
    }
    let report = maps::coverage(run)?;
    let population = coverage::population(root, &report, &inputs.inputs.files)?;
    let n = population.statements.len();
    let mut pass = Pass {
        population: &population,
        model: quality::model(),
        salt: salt(run),
        answers: vec![Vec::new(); n],
        orders: (0..n).map(|s| population.order(s, FIRST_TESTS)).collect(),
        wide: false,
        keys: BTreeMap::new(),
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
        return Ok(json!({
            "run": run.id, "dryRun": true, "statements": n, "answeredFromCache": n - undecided,
            "firstRound": {"requests": plan.len(), "inputTokens": bytes, "costUsd": bytes as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS},
            "note": "Statements every first test says is still passing are asked of more tests; on measured projects the whole pass cost 1.5 to 3 times the first round.",
        }));
    }
    let endpoint = quality::endpoint()?;
    let agent = quality::client();
    let batch_size = options.workers * 5;
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
        let batch = pass.plan(batch_size);
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
    Ok(listing(result, options.limit))
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

fn change_name(change: Change) -> &'static str {
    match change {
        Change::Invert => "condition inverted",
        Change::ReturnUndefined => "returns undefined",
        Change::ValueUndefined => "value becomes undefined",
        Change::ExpressionUndefined => "value becomes undefined",
        Change::Skip => "skipped",
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
                "change": change_name(st.change), "asserted": pass.asserted(s),
                "answer": best.map(|b| b.1),
                "test": best.map(|(t, _)| json!({"file": population.tests[t].file, "name": population.tests[t].name})),
                "testsAsked": pass.answers[s].len(), "testsRunningIt": st.tests.len(),
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
            "requests": requests, "answersReused": reused, "inputTokens": tokens,
            "costUsd": tokens as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS,
        },
        "basis": "Jev, from what each test ran; a statement is asserted when some test that runs it is judged to fail if it changes",
        "statements": statements,
    })
}

/// The run's saved result, or where the last one is when this run has none.
fn read(run: &StoredRun, runs: &[StoredRun], limit: Option<usize>) -> Result<Value, String> {
    let saved = |r: &StoredRun| {
        std::fs::read(r.directory.join(RESULT_FILE))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
    };
    if let Some(result) = saved(run) {
        return Ok(listing(result, limit));
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
        "assess": format!("npx supercov runs {} assertions assess", run.id),
        "note": "Answers from earlier assessments are reused when the same question comes up, so assessing a later run usually asks only about what changed; `assess --dry-run` shows how many questions it would send.",
    }))
}

/// The saved result trimmed for output: every statement is in the run's
/// assertion-coverage.json; the listing names the ones not asserted.
fn listing(mut result: Value, limit: Option<usize>) -> Value {
    let all = result["statements"].take();
    let not = all
        .as_array()
        .into_iter()
        .flatten()
        .filter(|s| s["asserted"] == false)
        .cloned()
        .collect::<Vec<_>>();
    let shown = limit.map_or(not.len(), |l| l.min(not.len()));
    result["notAsserted"] = json!(not[..shown]);
    result["notAssertedShown"] = json!(shown);
    result.as_object_mut().unwrap().remove("statements");
    result
}

fn render(data: &Value) -> String {
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
            "Run {}: {} executed statements, {} answered from the cache.\nFirst round: {} requests, about {} input tokens, ${:.4}.\n{}\n",
            data["run"].as_str().unwrap_or(""),
            data["statements"],
            data["answeredFromCache"],
            f["requests"],
            f["inputTokens"],
            f["costUsd"].as_f64().unwrap_or(0.0),
            data["note"].as_str().unwrap_or("")
        );
    }
    let s = &data["summary"];
    let mut out = format!(
        "Run {}: {} asserted ({} of {} executed statements)\n",
        data["run"].as_str().unwrap_or(""),
        s["percentage"]
            .as_f64()
            .map(|p| format!("{p}%"))
            .unwrap_or_else(|| "nothing".into()),
        s["asserted"],
        s["statements"]
    );
    let not = data["notAsserted"].as_array().cloned().unwrap_or_default();
    if s["notAsserted"].as_u64().unwrap_or(0) > 0 {
        out.push_str(&format!(
            "\n{} not asserted: no test that runs them was judged to fail if they changed.\n",
            s["notAsserted"]
        ));
        let mut file = "";
        for st in &not {
            let f = st["file"].as_str().unwrap_or("");
            if f != file {
                out.push_str(&format!("\n  {f}\n"));
                file = f;
            }
            out.push_str(&format!(
                "    {:>5}  {}  ({})\n",
                st["line"],
                st["text"].as_str().unwrap_or(""),
                st["change"].as_str().unwrap_or("")
            ));
        }
        if (not.len() as u64) < s["notAsserted"].as_u64().unwrap_or(0) {
            out.push_str(&format!(
                "\nShowing {} of {}; --all lists every one.\n",
                not.len(),
                s["notAsserted"]
            ));
        }
    }
    out.push_str(&format!(
        "\nAssessed by Jev: {} requests, {} input tokens, ${:.4}; {} answers reused from .supercov/assertions/.\n",
        s["requests"], s["inputTokens"], s["costUsd"].as_f64().unwrap_or(0.0), s["answersReused"]
    ));
    out
}
