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
        self as coverage, Change, FIRST_TESTS, LAST_TESTS, MAX_QUESTIONS, MAX_TESTS, MORE_TESTS,
        Population, THRESHOLD,
    },
    run_store::{StoredRun, discover_runs, select_run},
    source_manifest as maps,
};

const HELP: &str = r#"Usage: supercov runs <run> assertions [<file> | <file>:<line> | --test <name>] [--weak] [--offset N] [--limit N | --all] [--json]
       supercov runs <run> assertions assess [--changed] [--refresh] [--dry-run] [--workers N] [--json]
       Either takes --kind, --runner, --tag, --title, --test-file to read or assess some of the tests.

How much of the executed code the tests assert: a statement counts as asserted
when changing it would make at least one passing test that runs it fail. The
change follows the statement: an `if` inverted (each way), `return x`
returning undefined, a declaration's value becoming undefined, a boolean
literal flipped, a TypeScript `throw` throwing a bare Error, anything else
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
  assertions --weak         the files by how many of their statements are
                            asserted on the strength of an answer under 75%,
                            where a verdict is wrong more often; with a file,
                            those statements, the least certain first

--offset N    Start the listing at its Nth entry (default 0).
--limit N     List N entries (default 20); 0 lists every entry.
--all         List every entry.
--changed     (assess) Only the statements in code changed since the run,
              asked of every test that ran them rather than until one
              catches the change: what `tests affected` reads to say which
              tests check the change. Saved beside the run's assessment.
--refresh     (assess) Ask every question again instead of reusing saved
              answers, and keep the new answers. For an answer you have
              shown is wrong: a reused answer is the one Jev gave before.
--dry-run     (assess) Estimate the requests and cost; send nothing.
--workers N   (assess) Requests in flight at once (default 24). Each round
              plans the same requests whatever this is, so it changes only
              how long the pass takes.
--json        Structured output for integrations.

--kind, --runner, --tag, --title, --test-file
              Only the tests these pick, as every query reads them; a leading
              `!` leaves those out. `assess --tag @e2e` asks only those tests
              about the statements they ran, saves its result apart from the
              run's whole assessment, and keeps its answers for a later
              `assess`. Reading with a filter shows what those tests assert:
              a statement they were asked about and none catches is not
              asserted there; one only other tests were asked about is
              counted apart, with the `assess` that asks these tests.
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
///
/// `request` is the request asking test `t` about `asked`; a statement and
/// test whose request is over even so go in `refused`.
fn within_limits(
    request: impl Fn(usize, &[usize]) -> Value,
    refused: &mut BTreeSet<(usize, usize)>,
    batch: Vec<(usize, Vec<usize>)>,
) -> Vec<(usize, Vec<usize>)> {
    let mut out = Vec::new();
    let mut pending = batch;
    while let Some((t, asked)) = pending.pop() {
        let request = request(t, &asked);
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
        } else {
            // A plan asks a test about something, and a split leaves each
            // half something: this is the one statement left.
            refused.insert((asked[0], t));
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
    /// Ask again rather than reuse saved answers.
    refresh: bool,
    json: bool,
    target: Target,
    offset: usize,
    limit: Option<usize>,
    workers: usize,
    /// Only some of the run's tests: assessed, or read.
    filter: supercov_engine::coverage_report::TestFilter,
}

/// Where in the saved result a reading looks.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Overview,
    File(String),
    Line(String, u64),
    Test(String),
    /// The overview, by statements asserted on a weak answer.
    Weak,
    /// One file's statements asserted on a weak answer.
    WeakFile(String),
}

/// An asserted verdict rests on its strongest answer, and how strong that is
/// says how far to trust it. Checked by applying each change and running the
/// tests, an asserted verdict was wrong 22 times in 1,169 on an answer of
/// 0.75 or more and 33 times in 711 under it. A label, not a rule: raising
/// the bar for "asserted" to here was measured and made the verdicts worse.
const WEAK_ANSWER: f64 = 0.75;

fn weakly_asserted(statement: &Value) -> bool {
    statement["asserted"] == true
        && statement["answer"]
            .as_f64()
            .is_some_and(|p| p < WEAK_ANSWER)
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
        // On standard error with the mistake: standard output is the answer.
        Err(message) => {
            return crate::refuse(
                "runs.assertions",
                args.iter().any(|a| a == "--json"),
                message,
                HELP,
            );
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
        Err(message) => crate::refuse("runs.assertions", options.json, message, ""),
    }
}

fn parse(args: &[String]) -> Result<Options, String> {
    let mut o = Options {
        assess: false,
        changed: false,
        dry_run: false,
        refresh: false,
        json: false,
        target: Target::Overview,
        offset: 0,
        limit: Some(PAGE),
        workers: 24,
        filter: Default::default(),
    };
    let mut i = 0;
    let mut weak = false;
    if args.first().is_some_and(|a| a == "assess") {
        o.assess = true;
        i = 1;
    }
    while i < args.len() {
        let mut next = i + 1;
        if let Some(taken) = crate::test_filter_option(&args[i], args, &mut next, &mut o.filter) {
            taken?;
            i = next;
            continue;
        }
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
            "--refresh" => o.refresh = true,
            "--changed" => o.changed = true,
            "--json" => o.json = true,
            "--weak" if o.assess => {
                return Err(
                    "--weak reads a saved result: `assertions --weak` after the assessment".into(),
                );
            }
            "--weak" => weak = true,
            "--all" => o.limit = None,
            // `--limit 0` is every row, as `quality` and `security` spell it;
            // it was refused here, where the same thing was `--all` alone.
            "--limit" => {
                let rows = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<usize>().ok())
                    .ok_or("--limit needs a number of rows, or 0 for all of them")?;
                o.limit = (rows > 0).then_some(rows);
                i += 1;
            }
            "--workers" => {
                o.workers = args
                    .get(i + 1)
                    .and_then(|v| v.parse::<usize>().ok())
                    .filter(|v| *v > 0)
                    .ok_or("--workers needs a positive number")?
                    .min(64);
                i += 1;
            }
            other => return Err(format!("Unknown option: {other}")),
        }
        i += 1;
    }
    if weak {
        o.target = match o.target {
            Target::Overview => Target::Weak,
            Target::File(file) => Target::WeakFile(file),
            _ => {
                return Err(
                    "--weak lists a run's or a file's statements; give a file or nothing".into(),
                );
            }
        };
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
        let answers = supercov_engine::fault_fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
            .filter(|v| v["version"] == CACHE_VERSION)
            .and_then(|v| serde_json::from_value(v["answers"].clone()).ok())
            .unwrap_or_default();
        Cache { path, answers }
    }
    fn save(&self) -> Result<(), String> {
        let dir = self
            .path
            .parent()
            .expect("answers.json sits in .supercov/assertions");
        supercov_engine::fault_fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        let bytes = serde_json::to_vec(&json!({"version": CACHE_VERSION, "answers": self.answers}))
            .expect("plain data serializes");
        let temp = self.path.with_extension("json.tmp");
        supercov_engine::fault_fs::write(&temp, bytes).map_err(|e| e.to_string())?;
        supercov_engine::fault_fs::rename(&temp, &self.path).map_err(|e| e.to_string())
    }
}

struct Pass<'p> {
    population: &'p Population,
    model: &'static str,
    /// Per statement: (test, answer) in the order asked.
    answers: Vec<Vec<(usize, f64)>>,
    orders: Vec<Vec<usize>>,
    /// 0: the first tests. 1: more of them. 2: the last few, chosen for the
    /// statement.
    round: u8,
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
        let k = self.population.pair_key(statement, test, self.model);
        self.keys.insert((statement, test), k.clone());
        k
    }
    fn asserted(&self, statement: usize) -> bool {
        self.answers[statement].iter().any(|(_, p)| *p >= THRESHOLD)
    }
    fn limit(&self, statement: usize) -> usize {
        [FIRST_TESTS, MORE_TESTS, MAX_TESTS][usize::from(self.round)]
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
        // No saved answer to find: a key for every statement and test it
        // goes to first is not worth making.
        if cache.answers.is_empty() {
            return reused;
        }
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
        self.round += 1;
        for s in 0..self.population.statements.len() {
            if self.asserted(s) {
                continue;
            }
            if self.round == 1 {
                self.orders[s] = self.population.order(s, MORE_TESTS);
                continue;
            }
            // The tests already asked, then the last few.
            let mut asked = self.answers[s].iter().map(|(t, _)| *t).collect::<Vec<_>>();
            asked.extend(
                self.refused
                    .range((s, 0)..=(s, usize::MAX))
                    .map(|(_, t)| *t),
            );
            let last = self.population.last_order(s, &asked, LAST_TESTS);
            asked.extend(last);
            self.orders[s] = asked;
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

/// How a pass treats the answers it has and the ones it gets.
struct Asking {
    /// Read no saved answer.
    refresh: bool,
    /// Save each answer for later passes. An estimate answers its own
    /// questions and keeps none of them.
    keep: bool,
}

/// What one request came back with: the answer to each of its questions in
/// the order asked, where it gave one, and the input tokens it was charged.
type Reply = Result<(Vec<Option<f64>>, u64), String>;

struct Sent {
    requests: usize,
    tokens: u64,
    /// Saved answers taken up as the pass widened.
    reused: usize,
}

/// The pass from its first round to its last. `ask` answers a round's
/// requests: Jev for an assessment, a no to everything for the estimate of
/// what one can cost at most, so both plan, split and group their requests
/// the same way.
///
/// A round's requests are planned from the answers so far, so its size
/// decides how many questions early stopping saves; how many are in flight
/// decides only the wait. 40 is what eight workers planned; with 24 in flight
/// the three largest measured projects took half the time for the same cost.
fn run_pass(
    pass: &mut Pass,
    cache: &mut Cache,
    asking: Asking,
    mut ask: impl FnMut(&Pass, &[(usize, Vec<usize>)]) -> Result<Vec<Reply>, String>,
    mut after_round: impl FnMut(usize, usize),
) -> Result<Sent, String> {
    let population = pass.population;
    let n = population.statements.len();
    let mut sent = Sent {
        requests: 0,
        tokens: 0,
        reused: 0,
    };
    let mut failures = 0usize;
    loop {
        // Nothing left to ask in this round, or undecided statements no test
        // is left to ask about (the rest were over Jev's limit): widen, and
        // after the last widening stop.
        let planned = if (0..n).all(|s| !pass.undecided(s)) {
            Vec::new()
        } else {
            pass.plan(ROUND_REQUESTS)
        };
        if planned.is_empty() {
            if pass.round == 2 {
                break;
            }
            pass.widen();
            if !asking.refresh {
                sent.reused += pass.seed(cache);
            }
            continue;
        }
        let model = pass.model;
        let batch = within_limits(
            |t, asked| population.request(t, asked, model),
            &mut pass.refused,
            planned,
        );
        if batch.is_empty() {
            continue;
        }
        let replies = ask(pass, &batch)?;
        for ((t, asked), reply) in batch.iter().zip(replies) {
            sent.requests += 1;
            let (answers, tokens) = match reply {
                Ok(reply) => reply,
                Err(e) => {
                    failures += 1;
                    if failures > MAX_FAILURES {
                        // Only the pass that sends requests has one fail,
                        // and it keeps its answers.
                        cache.save()?;
                        return Err(format!("stopped after {failures} failed requests: {e}"));
                    }
                    continue;
                }
            };
            failures = 0;
            sent.tokens += tokens;
            for (&s, p) in asked.iter().zip(answers) {
                let Some(p) = p else { continue };
                pass.answers[s].push((*t, p));
                if asking.keep {
                    let k = pass.key(s, *t);
                    cache.answers.insert(k, p);
                }
            }
        }
        if asking.keep {
            cache.save()?;
        }
        after_round(sent.requests, (0..n).filter(|&s| pass.undecided(s)).count());
    }
    Ok(sent)
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
        let input = narrowed(run, assessment_input(run)?, &options.filter)?;
        let sources = snapshot
            .iter()
            .filter(|(file, _)| stored.manifest.files.contains_key(*file))
            .map(|(f, t)| (f.clone(), t.clone()))
            .collect();
        let population =
            coverage::population_from(root, &input, &sources, language)?.with_files(snapshot);
        return assess_changed(root, run, options, &population);
    }
    let input = narrowed(run, assessment_input(run)?, &options.filter)?;
    // Only the files the questions quote are checked against the run: the
    // source of each assessed statement and each test's own file. A file the
    // run merely read -- an .env only CI generates -- may be missing here.
    let needed = input
        .points
        .iter()
        .chain(&input.decisions)
        // A statement no test ran is not asked about.
        .filter(|point| !point.tests.is_empty())
        .map(|point| point.file.as_str())
        .chain(input.tests.iter().filter_map(|test| test.file.as_deref()))
        .collect::<BTreeSet<_>>();
    let inputs = maps::load_inputs_for(root, run, |file| needed.contains(file))?;
    let Some(language) = coverage::Language::from_manifest(&inputs.inputs.language) else {
        return Err(format!(
            "Assertion coverage supports JavaScript, TypeScript, Python, Ruby, Go, Rust, Java and Kotlin runs; this run is {}",
            inputs.inputs.language
        ));
    };
    let population = coverage::population_from(root, &input, &inputs.inputs.files, language)?;
    let n = population.statements.len();
    let mut pass = Pass {
        population: &population,
        model: quality::model(),
        answers: vec![Vec::new(); n],
        orders: (0..n).map(|s| population.order(s, FIRST_TESTS)).collect(),
        round: 0,
        keys: BTreeMap::new(),
        refused: BTreeSet::new(),
    };
    let mut cache = Cache::load(root);
    // A refreshed pass reads no saved answer; what it asks is saved over them.
    let mut reused = if options.refresh {
        0
    } else {
        pass.seed(&cache)
    };
    let key = quality::setting("TYPESAFE_API_KEY");
    let progress = !options.json && std::io::IsTerminal::is_terminal(&std::io::stderr());
    if options.dry_run {
        let undecided = (0..n).filter(|&s| pass.undecided(s)).count();
        let planned = pass.plan(usize::MAX);
        let model = pass.model;
        let plan = within_limits(
            |t, asked| population.request(t, asked, model),
            &mut pass.refused,
            planned,
        );
        let bytes: usize = plan
            .iter()
            .map(|(t, asked)| tokens(&population.request(*t, asked, pass.model)))
            .sum();
        // The most the pass can ask is the pass itself with every answer a
        // no: each statement goes to every test it widens to, in requests
        // planned, split and grouped as the rounds do it. Counting each
        // test's statements once instead fell well short where tests share
        // most of what they run, as end-to-end tests do (404 against the 741
        // one such suite sent): a statement goes to one test per round of
        // requests, so a test is asked again and again about what the others
        // did not settle.
        let most = run_pass(
            &mut pass,
            &mut cache,
            Asking {
                refresh: options.refresh,
                keep: false,
            },
            |pass, batch| {
                Ok(batch
                    .iter()
                    .map(|(t, asked)| {
                        let request = pass.population.request(*t, asked, pass.model);
                        Ok((vec![Some(0.0); asked.len()], tokens(&request) as u64))
                    })
                    .collect())
            },
            |_, _| {},
        )?;
        let most_requests = most.requests.max(plan.len());
        let most_tokens = (most.tokens as usize).max(bytes);
        let cost = |tokens: usize| tokens as f64 / 1e6 * quality::USD_PER_MILLION_INPUT_TOKENS;
        return Ok(json!({
            "run": run.id, "dryRun": true, "statements": n, "answeredFromCache": n - undecided,
            "filter": (!options.filter.is_empty()).then(|| options.filter.describe()),
            "firstRound": {"requests": plan.len(), "inputTokens": bytes, "costUsd": cost(bytes)},
            "atMost": {"requests": most_requests, "inputTokens": most_tokens, "costUsd": cost(most_tokens)},
            "note": "Statements their first tests are not judged to catch are asked of more tests, so the pass costs between the first round and the most, which is what it sends when no test is judged to catch anything.",
        }));
    }
    let endpoint = quality::endpoint()?;
    let agent = quality::client();
    let sent = run_pass(
        &mut pass,
        &mut cache,
        Asking {
            refresh: options.refresh,
            keep: true,
        },
        |pass, batch| {
            let key = key
                .as_deref()
                .ok_or("set TYPESAFE_API_KEY to ask Jev, or use --dry-run for an estimate")?;
            let bodies = batch
                .iter()
                .map(|(t, asked)| pass.population.request(*t, asked, pass.model))
                .collect::<Vec<_>>();
            Ok(send_all(&agent, endpoint, key, &bodies, options.workers)
                .into_iter()
                .zip(batch)
                .map(|(result, (_, asked))| {
                    let response = result?;
                    let answers = (0..asked.len())
                        .map(|i| {
                            (0..2)
                                .filter_map(|j| match response.answers.get(&format!("q{i}_{j}")) {
                                    Some(quality::Answer::Noul { noul }) => Some(*noul),
                                    _ => None,
                                })
                                .fold(None, |m: Option<f64>, p| Some(m.map_or(p, |m| m.max(p))))
                        })
                        .collect();
                    Ok((answers, response.usage.input_tokens))
                })
                .collect())
        },
        |requests, left| {
            if progress {
                eprintln!(
                    "[supercov] assertions: {requests} requests, {left} statements still being asked"
                );
            }
        },
    )?;
    reused += sent.reused;
    let (requests, tokens) = (sent.requests, sent.tokens);
    let mut result = summary(run, &population, &pass, requests, tokens, reused);
    // Some of the tests are assessed apart from the whole run, whose result
    // `tests affected` and every unfiltered reading go on reading.
    let saved_as = if options.filter.is_empty() {
        RESULT_FILE.to_owned()
    } else {
        result["filter"] = json!(options.filter.describe());
        result["filterArgs"] = json!(filter_args(&options.filter));
        filtered_result_file(&options.filter)
    };
    let _ = supercov_engine::fault_fs::write(
        run.directory.join(saved_as),
        serde_json::to_vec(&result).unwrap_or_default(),
    );
    // Every file the questions rendered, and the run's sources, as verified,
    // beside what an earlier pass kept: a pass over some of the tests read
    // fewer files than `assess --changed` may need.
    let mut kept = coverage::load_snapshot(&run.directory.join(SNAPSHOT_FILE)).unwrap_or_default();
    kept.extend(population.files_read());
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
                let i = statement["index"]
                    .as_u64()
                    .expect("each statement carries its index");
                changed.insert(i as usize);
            }
        }
    }
    let model = quality::model();
    let mut cache = Cache::load(root);
    let mut answers = BTreeMap::<usize, Vec<(usize, f64)>>::new();
    let mut wanted = BTreeMap::<usize, Vec<usize>>::new(); // test -> statements
    let mut keys = BTreeMap::<(usize, usize), String>::new();
    let mut reused = 0;
    for &s in &changed {
        for t in population.order(s, IMPACT_TESTS) {
            let key = population.pair_key(s, t, model);
            match cache.answers.get(&key).filter(|_| !options.refresh) {
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
                // Every pair planned was keyed above.
                cache.answers.insert(keys[&(s, *t)].clone(), p);
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
    let _ = supercov_engine::fault_fs::write(
        run.directory.join(IMPACT_FILE),
        serde_json::to_vec(&result).unwrap_or_default(),
    );
    Ok(json!({"run": run.id, "impact": true, "summary": result}))
}

/// `assess --changed`'s saved answers for a run, if any.
pub(crate) fn impact(run: &StoredRun) -> Option<Value> {
    supercov_engine::fault_fs::read(run.directory.join(IMPACT_FILE))
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
            // For a statement no asked test catches: how many tests that run
            // it were not asked, and the test files none was asked from.
            let not_asked = (!pass.asserted(s) && pass.answers[s].len() < st.tests.len()).then(|| {
                let asked = pass.answers[s].iter().map(|(t, _)| *t).collect::<BTreeSet<_>>();
                let mut files = BTreeMap::<&str, (usize, usize)>::new();
                for t in &st.tests {
                    let entry = files.entry(population.tests[*t].file.as_str()).or_default();
                    entry.0 += 1;
                    entry.1 += usize::from(asked.contains(t));
                }
                let mut untouched = files
                    .iter()
                    .filter(|(_, (_, asked))| *asked == 0)
                    .map(|(file, (tests, _))| (*tests, *file))
                    .collect::<Vec<_>>();
                untouched.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(b.1)));
                json!({
                    "tests": st.tests.len() - asked.len(),
                    "files": untouched.iter().map(|(tests, file)| json!({"file": file, "tests": tests})).collect::<Vec<_>>(),
                })
            });
            json!({
                "file": st.file, "line": st.line, "text": st.text.lines().next().unwrap_or("").trim(),
                "change": change_name(st.change, population.language), "replacement": st.replacement,
                "asserted": pass.asserted(s),
                "notAsked": not_asked,
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
/// What assessing reads from the run: stored at publication, or analysed
/// from the evidence for a run published before that.
fn assessment_input(run: &StoredRun) -> Result<coverage::AssessmentInput, String> {
    match supercov_engine::run_store::read_assessment_input(run) {
        Some(input) => Ok(input),
        None => Ok(coverage::assessment_input(&maps::coverage(run)?)),
    }
}

/// The assessment input with only the tests `filter` picks, as every query
/// picks them; all of it with no filter.
fn narrowed(
    run: &StoredRun,
    input: coverage::AssessmentInput,
    filter: &supercov_engine::coverage_report::TestFilter,
) -> Result<coverage::AssessmentInput, String> {
    if filter.is_empty() {
        return Ok(input);
    }
    let picked = crate::picked_tests(run, filter)?;
    let narrowed = input.only_tests(|test| picks(&picked, &test.name, test.file.as_deref()));
    if narrowed.points.is_empty() {
        return Err(format!(
            "the tests {} picks ran no assessed statement",
            filter.describe()
        ));
    }
    Ok(narrowed)
}

/// A test by its name and file: one with only the name the same is another
/// test.
fn picks(picked: &BTreeSet<(String, Option<String>)>, name: &str, file: Option<&str>) -> bool {
    picked.contains(&(name.to_owned(), file.map(str::to_owned)))
}

/// The options that give `filter` again, for the commands a reading names.
fn filter_args(filter: &supercov_engine::coverage_report::TestFilter) -> Vec<String> {
    let mut args = Vec::new();
    for (flag, value) in [("--kind", &filter.kind), ("--runner", &filter.runner)] {
        if let Some(value) = value {
            args.extend([flag.to_owned(), value.clone()]);
        }
    }
    for (flag, values) in [
        ("--tag", &filter.tags),
        ("--title", &filter.titles),
        ("--test-file", &filter.files),
    ] {
        for value in values {
            args.extend([flag.to_owned(), value.clone()]);
        }
    }
    args
}

/// ` --tag '@e2e'`: a reading's filter, for each command it names.
fn flags(data: &Value) -> String {
    data["filterArgs"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(|arg| {
            if arg.starts_with("--") {
                format!(" {arg}")
            } else {
                format!(" {}", quote(arg))
            }
        })
        .collect()
}

/// Where an assessment of only the tests `filter` picks is saved, beside the
/// run's whole one.
fn filtered_result_file(filter: &supercov_engine::coverage_report::TestFilter) -> String {
    let digest = Sha256::digest(filter.describe().as_bytes());
    let hex = digest
        .iter()
        .take(8)
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("assertion-coverage-{hex}.json")
}

/// The run's whole assessment read for only the tests a filter picks: each
/// statement those tests ran, with their answers alone. A statement they ran
/// that was asserted by another test before they were asked is not asked of
/// them, and says so.
fn narrow_result(
    mut result: Value,
    input: &coverage::AssessmentInput,
    picked: &BTreeSet<(String, Option<String>)>,
    filter: &supercov_engine::coverage_report::TestFilter,
) -> Value {
    let running = input
        .points
        .iter()
        .map(|point| {
            let tests = point
                .tests
                .iter()
                .filter_map(|t| input.tests.get(*t as usize))
                .filter(|test| picks(picked, &test.name, test.file.as_deref()))
                .collect::<Vec<_>>();
            ((point.file.as_str(), point.line as u64), tests)
        })
        .collect::<BTreeMap<_, _>>();
    let mut unasked = Vec::<String>::new();
    let statements = result["statements"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|mut statement| {
            let file = statement["file"].as_str()?.to_owned();
            let line = statement["line"].as_u64()?;
            let tests = running.get(&(file.as_str(), line)).filter(|t| !t.is_empty())?;
            let answers: Vec<Value> = statement["answers"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|answer| {
                    picks(
                        picked,
                        answer["name"].as_str().unwrap_or_default(),
                        answer["file"].as_str(),
                    )
                })
                .cloned()
                .collect();
            // None of these tests was asked about it: other tests answered
            // for the statement first. Not asserted would be a guess.
            if answers.is_empty() {
                unasked.push(file.clone());
                return None;
            }
            let best = answers.iter().max_by(|a, b| {
                let p = |v: &Value| v["p"].as_f64().unwrap_or(0.0);
                p(a).total_cmp(&p(b))
            });
            let asserted = best.is_some_and(|b| b["p"].as_f64().unwrap_or(0.0) >= THRESHOLD);
            statement["asserted"] = json!(asserted);
            statement["answer"] = best.map_or(Value::Null, |b| b["p"].clone());
            statement["test"] = best.map_or(Value::Null, |b| json!({"file": b["file"], "name": b["name"]}));
            statement["testsAsked"] = json!(answers.len());
            statement["testsRunningIt"] = json!(tests.len());
            statement["notAsked"] = if asserted || answers.len() >= tests.len() {
                Value::Null
            } else {
                let mut files = BTreeMap::<&str, usize>::new();
                for test in tests.iter().filter(|test| {
                    !answers.iter().any(|a| {
                        a["name"].as_str() == Some(test.name.as_str())
                            && a["file"].as_str() == test.file.as_deref()
                    })
                }) {
                    *files.entry(test.file.as_deref().unwrap_or("")).or_default() += 1;
                }
                json!({
                    "tests": tests.len() - answers.len(),
                    "files": files.iter().map(|(file, tests)| json!({"file": file, "tests": tests})).collect::<Vec<_>>(),
                })
            };
            statement["answers"] = json!(answers);
            Some(statement)
        })
        .collect::<Vec<_>>();
    let asserted = statements.iter().filter(|s| s["asserted"] == true).count();
    let total = statements.len();
    result["summary"]["statements"] = json!(total);
    result["summary"]["asserted"] = json!(asserted);
    result["summary"]["notAsserted"] = json!(total - asserted);
    result["summary"]["percentage"] = if total == 0 {
        Value::Null
    } else {
        json!((asserted as f64 * 1000.0 / total as f64).round() / 10.0)
    };
    // What reading these tests out of the whole assessment cannot say, and
    // the pass that asks them.
    result["summary"]["unasked"] = json!(unasked.len());
    result["summary"]["askUnasked"] = if unasked.is_empty() {
        Value::Null
    } else {
        json!(format!(
            "{} runs {} assertions assess{}",
            crate::launcher_command(),
            quote(result["run"].as_str().unwrap_or_default()),
            flags(&json!({"filterArgs": filter_args(filter)}))
        ))
    };
    result["statements"] = json!(statements);
    result["filter"] = json!(filter.describe());
    result["filterArgs"] = json!(filter_args(filter));
    result
}

pub(crate) fn saved(run: &StoredRun) -> Option<Value> {
    supercov_engine::fault_fs::read(run.directory.join(RESULT_FILE))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok())
}

/// The lines of the statements judged asserted in a run, by file; `None`
/// when the run has no saved assessment.
pub(crate) fn asserted_lines(run: &StoredRun) -> Option<BTreeMap<String, BTreeSet<u64>>> {
    Some(asserted_in(&saved(run)?))
}

/// The lines of the statements an assessment judged asserted, by file.
fn asserted_in(result: &Value) -> BTreeMap<String, BTreeSet<u64>> {
    let mut out = BTreeMap::<String, BTreeSet<u64>>::new();
    for s in result["statements"].as_array().into_iter().flatten() {
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
    if !options.filter.is_empty() {
        // An assessment of exactly these tests, else the whole one read for
        // them; the filter is checked against the run either way.
        let picked = crate::picked_tests(run, &options.filter)?;
        let own = supercov_engine::fault_fs::read(
            run.directory.join(filtered_result_file(&options.filter)),
        )
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok());
        let result = match own {
            Some(result) => Some(result),
            None => saved(run)
                .map(|whole| {
                    assessment_input(run)
                        .map(|input| narrow_result(whole, &input, &picked, &options.filter))
                })
                .transpose()?,
        };
        if let Some(result) = result {
            return view(result, &options.target, options.offset, options.limit);
        }
    } else if let Some(result) = saved(run) {
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
        "assess": format!(
            "{} runs {} assertions assess{}",
            crate::launcher_command(),
            run.id,
            flags(&json!({"filterArgs": filter_args(&options.filter)}))
        ),
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
        Target::Overview | Target::Weak => {
            let by_weak = *target == Target::Weak;
            let mut files = BTreeMap::<String, (u64, u64, u64)>::new();
            for statement in &all {
                let entry = files
                    .entry(statement["file"].as_str().unwrap_or("").to_owned())
                    .or_default();
                entry.0 += 1;
                entry.1 += u64::from(statement["asserted"] == true);
                entry.2 += u64::from(weakly_asserted(statement));
            }
            let weak: u64 = files.values().map(|counts| counts.2).sum();
            let mut rows = files
                .into_iter()
                .filter(|(_, counts)| !by_weak || counts.2 > 0)
                .map(|(file, (total, asserted, weak))| {
                    json!({
                        "file": file,
                        "statements": total,
                        "asserted": asserted,
                        "notAsserted": total - asserted,
                        "weaklyAsserted": weak,
                        "percentage": percentage(asserted, total),
                    })
                })
                .collect::<Vec<_>>();
            let first = if by_weak {
                "weaklyAsserted"
            } else {
                "notAsserted"
            };
            rows.sort_by(|a, b| {
                b[first]
                    .as_u64()
                    .cmp(&a[first].as_u64())
                    .then_with(|| a["file"].as_str().cmp(&b["file"].as_str()))
            });
            let total = rows.len();
            let shown = window(rows, offset, limit);
            // Counted when read, so a result saved before the count existed has it.
            if let Some(summary) = data.get_mut("summary").and_then(Value::as_object_mut) {
                summary.insert("weaklyAsserted".into(), json!(weak));
            }
            data.insert("weakAnswerUnder".into(), json!(WEAK_ANSWER));
            data.insert("view".into(), json!(if by_weak { "weak" } else { "files" }));
            data.insert("page".into(), page(offset, limit, shown.len(), total));
            data.insert("files".into(), json!(shown));
        }
        Target::WeakFile(file) => {
            let mut weak = all
                .iter()
                .filter(|statement| statement["file"] == file.as_str())
                .filter(|statement| weakly_asserted(statement))
                .map(slim)
                .collect::<Vec<_>>();
            if !all
                .iter()
                .any(|statement| statement["file"] == file.as_str())
            {
                return Err(format!(
                    "no assessed statement in {file}; `{} runs {run} assertions` lists the assessed files",
                    crate::launcher_command()
                ));
            }
            // The least certain first: where a check is most likely to pay.
            weak.sort_by(|a, b| {
                a["answer"]
                    .as_f64()
                    .unwrap_or(0.0)
                    .total_cmp(&b["answer"].as_f64().unwrap_or(0.0))
                    .then_with(|| a["line"].as_u64().cmp(&b["line"].as_u64()))
            });
            let total = weak.len();
            let shown = window(weak, offset, limit);
            data.insert("weakAnswerUnder".into(), json!(WEAK_ANSWER));
            data.insert("view".into(), json!("weakFile"));
            data.insert("file".into(), json!(file));
            data.insert("page".into(), page(offset, limit, shown.len(), total));
            data.insert("weaklyAsserted".into(), json!(shown));
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
            let weak = here.iter().filter(|s| weakly_asserted(s)).count();
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
                    "weaklyAsserted": weak,
                    "percentage": percentage(asserted, here.len() as u64),
                }),
            );
            data.insert("weakAnswerUnder".into(), json!(WEAK_ANSWER));
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
                    // The assessed tests are the only names this can take.
                    let assessed = all
                        .iter()
                        .flat_map(|s| s["answers"].as_array().cloned().unwrap_or_default())
                        .filter_map(|a| {
                            Some((
                                a["file"].as_str().unwrap_or("").to_owned(),
                                a["name"].as_str()?.to_owned(),
                            ))
                        })
                        .collect::<BTreeSet<_>>();
                    let listed = assessed
                        .iter()
                        .take(10)
                        .map(|(file, name)| format!("  {name} ({file})"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    let more = assessed.len().saturating_sub(10);
                    return Err(if assessed.is_empty() {
                        format!("no assessed test is named {query}; this assessment asked no test")
                    } else if more > 0 {
                        format!(
                            "no assessed test is named {query}; a test that ran is asked nothing when other tests catch each statement it runs first. The assessed tests include:\n{listed}\n  and {more} more"
                        )
                    } else {
                        format!(
                            "no assessed test is named {query}; a test that ran is asked nothing when other tests catch each statement it runs first. The assessed tests are:\n{listed}"
                        )
                    });
                }
                1 => tests.into_iter().next().unwrap(),
                n => {
                    let names = tests
                        .iter()
                        .take(10)
                        .map(|(file, name)| format!("  {name} ({file})"))
                        .collect::<Vec<_>>()
                        .join("\n");
                    // One name in several files is told apart by its file.
                    let how = if tests
                        .iter()
                        .all(|(_, name)| name == &tests.first().unwrap().1)
                    {
                        "add --test-file with one's file"
                    } else {
                        "give one's full name"
                    };
                    return Err(format!("{n} assessed tests match {query}; {how}:\n{names}"));
                }
            };
            let mut items = all
                .iter()
                .filter_map(|s| {
                    // The test as `named` read it: its file, or none.
                    let answer = s["answers"].as_array()?.iter().find(|a| {
                        (a["file"].as_str().unwrap_or(""), a["name"].as_str())
                            == (test_file.as_str(), Some(test_name.as_str()))
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
                // say where the next one starts. A line's view lists one
                // statement, the first on the line, which the assessment
                // asks about alone.
                let Some(key) = ["files", "notAsserted", "statements"]
                    .into_iter()
                    .find(|key| data[*key].as_array().is_some_and(|list| list.len() > 1))
                else {
                    return Err(size);
                };
                let listed = data[key].as_array().map_or(0, Vec::len);
                let keep = listed / 2;
                data[key] = json!(data[key].as_array().unwrap()[..keep]);
                let page = data["page"]
                    .as_object_mut()
                    .expect("every listing has a page");
                let offset = page["offset"].as_u64().unwrap_or(0);
                page.insert("returned".into(), json!(keep));
                page.insert("nextOffset".into(), json!(offset + keep as u64));
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
        // Nothing the run measured changed: nothing to ask, and nothing for
        // `tests affected` to say.
        if s["changed"].as_u64() == Some(0) {
            return format!(
                "Run {}: no statement the run assessed has changed since it; nothing was asked.\n",
                data["run"].as_str().unwrap_or("")
            );
        }
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
        Some("weak") => render_weak(run, data),
        Some("weakFile") => render_weak_file(run, data),
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
        // A test whose own code is over Jev's limit is never asked: what it
        // would catch is unknown, not missing.
        if let Some(n) = s["pairsOverLimit"].as_u64().filter(|n| *n > 0) {
            out.push_str(&format!(
                "Not asked: {n} statement(s) of a test whose request is over Jev's limit even with one question; that test was not judged on them.\n"
            ));
        }
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
    if returned == 0 && total > 0 {
        return format!("showing none of {total}: --offset {offset} is past the end\n");
    }
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
    if s["statements"].as_u64() == Some(0)
        && let (Some(unasked), Some(ask), Some(filter)) = (
            s["unasked"].as_u64().filter(|n| *n > 0),
            s["askUnasked"].as_str(),
            data["filter"].as_str(),
        )
    {
        return format!(
            "Run {run}: not assessed for the tests {filter} picks: the {unasked} statement(s) they run were asked of other tests, which answered first. `{ask}` asks these tests.\n"
        );
    }
    if s["statements"].as_u64() == Some(0)
        && let Some(filter) = data["filter"].as_str()
    {
        return format!("Run {run}: the tests {filter} picks ran no assessed statement.\n");
    }
    let mut out = format!(
        "Run {run}: {} asserted ({} of {} executed statements){}\n",
        share(&s["percentage"]),
        s["asserted"],
        s["statements"],
        data["filter"]
            .as_str()
            .map(|filter| format!(", by the tests {filter} picks"))
            .unwrap_or_default()
    );
    if let (Some(unasked), Some(ask)) = (
        s["unasked"].as_u64().filter(|n| *n > 0),
        s["askUnasked"].as_str(),
    ) {
        out.push_str(&format!(
            "{unasked} other statement(s) these tests run were not asked of them, because other tests answered for them first; `{ask}` asks these tests.\n"
        ));
    }
    out.push_str(&weak_line(
        &s["weaklyAsserted"],
        &format!(
            "{} runs {} assertions{} --weak",
            crate::launcher_command(),
            quote(run),
            flags(data)
        ),
    ));
    let files = data["files"].as_array().cloned().unwrap_or_default();
    if files.is_empty() {
        if data["page"]["total"].as_u64() > Some(0) {
            out.push_str(&page_footer(data, ""));
        }
        return out;
    }
    out.push_str(&format!(
        "\nFiles, most statements not asserted first: a statement is not asserted when none of the tests asked about it was judged to fail if it changed. Up to {MAX_TESTS} of the tests that run a statement are asked, so a test never asked can still catch it.\n NOT ASSERTED  ASSERTED  FILE\n",
    ));
    files.iter().for_each(|file| {
        out.push_str(&format!(
            " {:>12}  {:>8}  {}\n",
            file["notAsserted"].as_u64().unwrap_or(0),
            format!("{}/{}", file["asserted"], file["statements"]),
            file["file"].as_str().unwrap_or("")
        ));
    });
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions{}",
            crate::launcher_command(),
            quote(run),
            flags(data)
        ),
    ));
    if let Some(first) = files.iter().find(|f| f["notAsserted"].as_u64() > Some(0)) {
        out.push_str(&format!(
            "inspect a file: {} runs {} assertions{} {}\n",
            crate::launcher_command(),
            quote(run),
            flags(data),
            quote(first["file"].as_str().unwrap_or(""))
        ));
    }
    out
}

/// How many asserted verdicts rest on a weak answer, and where to read them.
/// "98.7% asserted" reads as a measurement; a third of it can be one test's
/// answer a little over even.
fn weak_line(count: &Value, command: &str) -> String {
    match count.as_u64() {
        Some(n) if n > 0 => format!(
            "{n} of the asserted rest on an answer under {:.0}%, where a verdict is wrong more often: `{command}`\n",
            WEAK_ANSWER * 100.0
        ),
        _ => String::new(),
    }
}

fn render_weak(run: &str, data: &Value) -> String {
    let s = &data["summary"];
    let mut out = format!(
        "Run {run}: {} of {} asserted statements rest on an answer under {:.0}%\n",
        s["weaklyAsserted"],
        s["asserted"],
        WEAK_ANSWER * 100.0
    );
    let files = data["files"].as_array().cloned().unwrap_or_default();
    let Some(first) = files.first() else {
        if data["page"]["total"].as_u64() > Some(0) {
            out.push_str(&page_footer(data, ""));
        }
        return out;
    };
    out.push_str("\nAn asserted statement is one some test was judged to catch; the judgment is the test's strongest answer. Checked by applying each change, a verdict on an answer under 75% was wrong about 1 time in 20, and one on a stronger answer about 1 in 50. Files, most such statements first:\n  WEAK  ASSERTED  FILE\n");
    files.iter().for_each(|file| {
        out.push_str(&format!(
            " {:>5}  {:>8}  {}\n",
            file["weaklyAsserted"].as_u64().unwrap_or(0),
            file["asserted"].as_u64().unwrap_or(0),
            file["file"].as_str().unwrap_or("")
        ));
    });
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions{} --weak",
            crate::launcher_command(),
            quote(run),
            flags(data)
        ),
    ));
    out.push_str(&format!(
        "inspect a file: {} runs {} assertions{} {} --weak\n",
        crate::launcher_command(),
        quote(run),
        flags(data),
        quote(first["file"].as_str().unwrap_or(""))
    ));
    out
}

fn render_weak_file(run: &str, data: &Value) -> String {
    let file = data["file"].as_str().unwrap_or("");
    let weak = data["weaklyAsserted"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut out = format!(
        "Run {run}: {file}: {} asserted statement(s) rest on an answer under {:.0}%\n",
        data["page"]["total"],
        WEAK_ANSWER * 100.0
    );
    let Some(first) = weak.first() else {
        if data["page"]["total"].as_u64() > Some(0) {
            out.push_str(&page_footer(data, ""));
        }
        return out;
    };
    out.push_str("\nThe least certain first. Each was settled by the test named, at the answer shown; the tests not asked were not judged.\n  LINE  STATEMENT  (change)  answer  tests asked  test\n");
    weak.iter().for_each(|st| {
        out.push_str(&format!(
            " {:>5}  {}  ({})  {:.0}%  {} of {}  {}\n",
            st["line"].as_u64().unwrap_or(0),
            st["text"].as_str().unwrap_or(""),
            st["change"].as_str().unwrap_or(""),
            st["answer"].as_f64().unwrap_or(0.0) * 100.0,
            st["testsAsked"],
            st["testsRunningIt"],
            st["test"]["name"].as_str().unwrap_or("")
        ));
    });
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions{} {} --weak",
            crate::launcher_command(),
            quote(run),
            flags(data),
            quote(file)
        ),
    ));
    out.push_str(&format!(
        "inspect a statement: {} runs {} assertions{} {}\n",
        crate::launcher_command(),
        quote(run),
        flags(data),
        quote(&format!("{file}:{}", first["line"]))
    ));
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
    let weak = weak_line(
        &s["weaklyAsserted"],
        &format!(
            "{} runs {} assertions{} {} --weak",
            crate::launcher_command(),
            quote(run),
            flags(data),
            quote(file)
        ),
    );
    if s["notAsserted"].as_u64() == Some(0) {
        out.push_str("Every statement this file ran is asserted.\n");
        out.push_str(&weak);
        return out;
    }
    out.push_str(&weak);
    let Some(first) = not.first() else {
        out.push_str(&page_footer(data, ""));
        return out;
    };
    out.push_str(&format!("\nNot asserted: none of the tests asked was judged to fail if they changed. Up to {MAX_TESTS} of the tests that run a statement are asked; one never asked can still catch it.\n  LINE  STATEMENT  (change)  tests asked  closest test\n"));
    not.iter().for_each(|st| {
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
    });
    out.push_str(&page_footer(
        data,
        &format!(
            "{} runs {} assertions{} {}",
            crate::launcher_command(),
            quote(run),
            flags(data),
            quote(file)
        ),
    ));
    out.push_str(&format!(
        "inspect a statement: {} runs {} assertions{} {}\n",
        crate::launcher_command(),
        quote(run),
        flags(data),
        quote(&format!("{file}:{}", first["line"]))
    ));
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
        // Where a test that catches it may already be: the files none of
        // whose tests were asked, before anyone writes one that exists.
        let untouched = st["notAsked"]["files"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        if !untouched.is_empty() {
            const NAMED: usize = 5;
            let named = untouched
                .iter()
                .take(NAMED)
                .map(|f| format!("{} ({})", f["file"].as_str().unwrap_or(""), f["tests"]))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "    Not asked: {} test(s). No test was asked from {} file(s) that run it: {named}{}\n",
                st["notAsked"]["tests"],
                untouched.len(),
                if untouched.len() > NAMED {
                    format!(", and {} more", untouched.len() - NAMED)
                } else {
                    String::new()
                }
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
            "{} runs {} assertions{} --test {}",
            crate::launcher_command(),
            quote(run),
            flags(data),
            quote(name)
        ),
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A page past the end says so, an empty listing shows nothing of
    /// nothing, and the next page names a limit other than the default.
    #[test]
    fn a_page_footer_reads_past_the_end_an_empty_listing_and_a_chosen_limit() {
        let footer = |page: Value| page_footer(&json!({ "page": page }), "assertions");
        assert_eq!(
            footer(json!({"offset": 40, "returned": 0, "total": 3})),
            "showing none of 3: --offset 40 is past the end\n"
        );
        assert_eq!(
            footer(json!({"offset": 0, "returned": 0, "total": 0})),
            "showing 0-0 of 0\n"
        );
        assert_eq!(
            footer(json!({"offset": 0, "returned": 2, "total": 5, "nextOffset": 2, "limit": 2})),
            "showing 1-2 of 5\nnext page: assertions --offset 2 --limit 2\n"
        );
    }

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

    /// A request over the limit for all its questions is split by them, and
    /// one over it with a single question is refused for that statement.
    #[test]
    fn a_request_over_the_limit_is_split_and_one_question_over_it_refused() {
        // Just over 20,000 tokens: two fit beside the state, three do not.
        let mut words = 1_000;
        let question = loop {
            let text = "a ".repeat(words);
            if tokens(&json!(text)) > 20_000 {
                break text;
            }
            words += 1_000;
        };
        // Statements 0 to 2 are each one such question; statement 9 is too
        // long on its own.
        let request = |_t: usize, asked: &[usize]| {
            let questions = asked
                .iter()
                .map(|s| {
                    let text = if *s == 9 {
                        question.repeat(2)
                    } else {
                        question.clone()
                    };
                    (s.to_string(), json!(text))
                })
                .collect::<serde_json::Map<_, _>>();
            json!({"state": "the test", "questions": questions})
        };
        let mut refused = BTreeSet::new();
        let mut planned = within_limits(
            request,
            &mut refused,
            vec![(7, vec![0, 1, 2]), (8, vec![9])],
        );
        planned.sort();
        assert_eq!(planned, [(7, vec![0]), (7, vec![1, 2])]);
        assert_eq!(refused, BTreeSet::from([(9, 8)]));
    }

    /// Only a statement judged asserted and saying where it is counts.
    #[test]
    fn asserted_lines_are_those_judged_asserted_that_say_where_they_are() {
        let result = json!({"statements": [
            {"file": "a.js", "line": 1, "asserted": true},
            {"file": "a.js", "line": 2, "asserted": false},
            {"file": "a.js", "asserted": true},
            {"line": 4, "asserted": true},
        ]});
        assert_eq!(
            asserted_in(&result),
            BTreeMap::from([("a.js".to_owned(), BTreeSet::from([1]))])
        );
    }

    /// A saved result from before the summary existed, or a statement that
    /// is not an object, is shown as it is.
    #[test]
    fn a_result_without_a_summary_or_with_an_odd_statement_is_shown_as_it_is() {
        assert_eq!(slim(&json!("x")), json!("x"));
        let result = json!({"run": "run_x", "statements": [
            {"file": "a.js", "line": 1, "text": "x;", "change": "skipped", "asserted": true, "answer": 0.6, "answers": []},
        ]});
        let files = view(result, &Target::Overview, 0, Some(PAGE)).unwrap();
        assert!(files.get("summary").is_none(), "{files}");
        assert_eq!(files["files"][0]["file"], "a.js");
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

    /// From a real assessment: 98.7% asserted, and twelve of the asserted
    /// were noticed by no test when the change was made. Each had been
    /// settled by one to three tests on an answer between 0.51 and 0.77.
    #[test]
    fn asserted_on_a_weak_answer_is_counted_and_listed_least_certain_first() {
        let statement = |file: &str, line: u64, asserted: bool, answer: f64| json!({"file": file, "line": line, "text": "x;", "change": "skipped", "asserted": asserted, "answer": answer, "testsAsked": 1, "testsRunningIt": 17, "test": {"file": "t.js", "name": "one"}, "answers": [{"file": "t.js", "name": "one", "p": answer}]});
        let result = json!({"run": "run_x", "summary": {"asserted": 5, "statements": 6, "percentage": 83.3}, "statements": [
            statement("peer.js", 90, true, 0.69),
            statement("peer.js", 433, true, 0.51),
            statement("peer.js", 12, true, 0.93),
            statement("peer.js", 13, false, 0.31),
            statement("catalog.js", 116, true, 0.53),
            statement("solid.js", 1, true, 0.75),
        ]});
        // The overview says how much of the share is a weak answer.
        let files = view(result.clone(), &Target::Overview, 0, Some(PAGE)).unwrap();
        assert_eq!(files["summary"]["weaklyAsserted"], 3);
        let text = render(&files);
        assert!(text.contains("3 of the asserted rest on an answer under 75%, where a verdict is wrong more often: `supercov runs 'run_x' assertions --weak`"), "{text}");
        // By file, most first, and only the files that have any.
        let weak = view(result.clone(), &Target::Weak, 0, Some(PAGE)).unwrap();
        let listed = weak["files"].as_array().unwrap();
        assert_eq!(listed.len(), 2, "{weak}");
        assert_eq!(listed[0]["file"], "peer.js");
        assert_eq!(listed[0]["weaklyAsserted"], 2);
        assert!(
            render(&weak).contains("     2         3  peer.js"),
            "{}",
            render(&weak)
        );
        // One file's, the least certain first, without the answers.
        let file = view(
            result.clone(),
            &Target::WeakFile("peer.js".into()),
            0,
            Some(PAGE),
        )
        .unwrap();
        let lines = file["weaklyAsserted"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["line"].as_u64().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines, [433, 90]);
        assert!(file["weaklyAsserted"][0].get("answers").is_none());
        let text = render(&file);
        assert!(
            text.contains("   433  x;  (skipped)  51%  1 of 17  one"),
            "{text}"
        );
        assert!(
            text.contains("inspect a statement: supercov runs 'run_x' assertions 'peer.js:433'"),
            "{text}"
        );
        // A file's own page points at them, also when nothing is not asserted.
        let solid = view(
            result.clone(),
            &Target::File("catalog.js".into()),
            0,
            Some(PAGE),
        )
        .unwrap();
        assert!(render(&solid).contains("Every statement this file ran is asserted.\n1 of the asserted rest on an answer under 75%"), "{}", render(&solid));
        let none = view(
            result.clone(),
            &Target::WeakFile("solid.js".into()),
            0,
            Some(PAGE),
        )
        .unwrap();
        assert_eq!(none["page"]["total"], 0);
        assert!(view(result, &Target::WeakFile("c.js".into()), 0, None).is_err());
        assert!(parse(&["--weak".into(), "--test".into(), "one".into()]).is_err());
        assert!(parse(&["assess".into(), "--weak".into()]).is_err());
        assert_eq!(
            parse(&["a.js".into(), "--weak".into()]).unwrap().target,
            Target::WeakFile("a.js".into())
        );
        // Every row, spelled as `quality` spells it.
        assert_eq!(parse(&["--limit".into(), "0".into()]).unwrap().limit, None);
    }

    /// A statement eighteen tests were asked about, all from one test file,
    /// while fourteen other files ran it: the reader was not told where the
    /// test that catches it might already be.
    #[test]
    fn a_statement_names_the_test_files_none_of_whose_tests_were_asked() {
        let result = json!({"run": "run_x", "summary": {}, "statements": [{
            "file": "request.js", "line": 75, "text": "transport.onerror = (error) =>", "change": "skipped",
            "asserted": false, "answer": 0.36, "testsAsked": 2, "testsRunningIt": 132,
            "answers": [{"file": "http.test.js", "name": "one", "p": 0.36}, {"file": "http.test.js", "name": "two", "p": 0.2}],
            "notAsked": {"tests": 130, "files": [
                {"file": "relay.test.js", "tests": 40}, {"file": "safety.test.js", "tests": 22}, {"file": "a.test.js", "tests": 9},
                {"file": "b.test.js", "tests": 8}, {"file": "c.test.js", "tests": 7}, {"file": "d.test.js", "tests": 4}, {"file": "e.test.js", "tests": 1},
            ]},
        }]});
        let line = view(result, &Target::Line("request.js".into(), 75), 0, None).unwrap();
        let text = render(&line);
        assert!(text.contains("    Not asked: 130 test(s). No test was asked from 7 file(s) that run it: relay.test.js (40), safety.test.js (22), a.test.js (9), b.test.js (8), c.test.js (7), and 2 more\n"), "{text}");
    }

    /// Each option is refused where it does not belong, without its value,
    /// or beside a second target; the rest set what they say.
    #[test]
    fn assertion_options_are_refused_where_they_do_not_belong() {
        let parsed = |args: &[&str]| parse(&args.iter().map(|a| a.to_string()).collect::<Vec<_>>());
        for (args, said) in [
            (
                &["--dry-run"][..],
                "--dry-run belongs to `assertions assess`",
            ),
            (
                &["--workers", "2"][..],
                "--workers belongs to `assertions assess`",
            ),
            (
                &["--changed"][..],
                "--changed belongs to `assertions assess`",
            ),
            (
                &["assess", "--test", "x"][..],
                "--test reads a saved result",
            ),
            (&["assess", "--all"][..], "--all reads a saved result"),
            (&["--test"][..], "--test needs a test name"),
            (&["--test", ""][..], "--test needs a test name"),
            (&["a.js", "--test", "x"][..], "not more than one"),
            (&["a.js", "b.js"][..], "not more than one"),
            (&["--offset", "x"][..], "--offset needs a number"),
            (&["--limit", "-1"][..], "--limit needs a number of rows"),
            (
                &["assess", "--workers", "0"][..],
                "--workers needs a positive number",
            ),
            (&["assess", "a.js"][..], "Unknown option: a.js"),
            (&["--bogus"][..], "Unknown option: --bogus"),
            (&["a.js:3", "--weak"][..], "give a file or nothing"),
        ] {
            let error = parsed(args).err().unwrap_or_default();
            assert!(error.contains(said), "{args:?}: {error}");
        }
        let assess = parsed(&[
            "assess",
            "--dry-run",
            "--refresh",
            "--changed",
            "--workers",
            "999",
            "--json",
        ])
        .unwrap();
        assert!(assess.assess && assess.dry_run && assess.refresh && assess.changed && assess.json);
        assert_eq!(assess.workers, 64);
        let read = parsed(&["--all", "--offset", "3"]).unwrap();
        assert_eq!((read.limit, read.offset), (None, 3));
        assert_eq!(parsed(&["--weak"]).unwrap().target, Target::Weak);
        assert_eq!(parsed(&["--limit", "5"]).unwrap().limit, Some(5));
    }
}
