//! The strong pass for Go (`docs/design/strong-mutation.md`, Go): every
//! mutant a plan runs, against its candidate tests, in test binaries built
//! once.
//!
//! The pass copies the project, writes every mutant it runs into its file
//! behind a switch (`go_instrumenter::strong_rewrite`), and builds each test
//! package's binary once (`go test -c`). One mutant is active per process
//! (`SUPERCOV_ACTIVE_MUTANT`), so a mutant's run is its candidate tests in
//! the binary of each package they are in, by exact name (`-test.run
//! '^(TestA|TestB)$'`), stopping at the first failure (`-test.failfast`).
//!
//! Before anything is built, the module's own type checker says which
//! mutants compile (`runtime/go/mutants`, go/types over the package and its
//! tests, as `go test` compiles them): a mutant that does not is `unviable`
//! and gets no switch. A switch the compiler still refuses is found by
//! building, at the site the error is in, and its mutants are built one
//! each, from the file with only that mutant in place.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::fault_process::Launch;
use crate::go_instrumenter::{RUNTIME_IMPORT, SiteFacts, StrongFile, StrongMutant};

/// What the Go driver is asked to do.
pub struct GoStrongRequest<'a> {
    /// The project root: the module, or the directory a `go.work` is in.
    pub root: &'a Path,
    /// The run's test command, for the build tags and test flags it gives
    /// `go test`.
    pub command: &'a [String],
    /// The plan (`plan.json`) and where the verdicts go (`results.jsonl`).
    pub plan: &'a Path,
    pub results: &'a Path,
    /// How many test binaries run at once.
    pub workers: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Plan {
    tests: Vec<PlanTest>,
    mutants: Vec<PlanMutant>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanTest {
    id: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PlanMutant {
    id: String,
    file: String,
    index: usize,
    #[serde(default)]
    decided: Option<Value>,
    #[serde(default)]
    candidates: Vec<PlanCandidate>,
}

#[derive(Deserialize)]
struct PlanCandidate {
    test: usize,
}

/// A test's package directory and its name, from its id
/// (`<package directory>::<name>`).
fn split_test(id: &str) -> Option<(&str, &str)> {
    id.rsplit_once("::")
}

/// One verdict, as `results.jsonl` holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Verdict {
    status: &'static str,
    killed_by: Option<String>,
    tests: u64,
    duration_ms: u64,
    reason: Option<String>,
}

impl Verdict {
    fn of(status: &'static str, reason: impl Into<String>) -> Self {
        Self {
            status,
            killed_by: None,
            tests: 0,
            duration_ms: 0,
            reason: Some(reason.into()),
        }
    }
}

/// The flags of the run's `go test` the pass keeps: build flags for `go
/// test -c` and test flags for the binary.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GoFlags {
    /// The `go` the command runs (`go`, or the path it names).
    pub program: String,
    pub build: Vec<String>,
    pub test: Vec<String>,
    pub tags: String,
}

/// The flags `command` gives `go test` that change what is built or how
/// tests run: `-tags`, `-race`, `-mod`, `-trimpath`, `-ldflags`, and
/// `-short`, `-cpu`, `-parallel` for the binary. The rest (`-run`, `-v`,
/// `-count`, `-cover`, `-timeout`) the pass sets itself, or does not need.
pub fn go_flags(command: &[String]) -> GoFlags {
    let mut flags = GoFlags {
        program: "go".into(),
        ..GoFlags::default()
    };
    let Some(test) = command
        .windows(2)
        .position(|pair| pair[1] == "test" && pair[0].rsplit('/').next() == Some("go"))
    else {
        return flags;
    };
    flags.program = command[test].clone();
    let mut arguments = command[test + 2..].iter().peekable();
    while let Some(argument) = arguments.next() {
        let Some(flag) = argument.strip_prefix('-') else {
            continue;
        };
        let flag = flag.strip_prefix('-').unwrap_or(flag);
        let (name, value) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        let mut value_of = |value: Option<String>| value.or_else(|| arguments.next().cloned());
        match name {
            "tags" => {
                if let Some(tags) = value_of(value) {
                    flags.build.push(format!("-tags={tags}"));
                    flags.tags = tags;
                }
            }
            "mod" | "ldflags" => {
                if let Some(value) = value_of(value) {
                    flags.build.push(format!("-{name}={value}"));
                }
            }
            "race" | "trimpath" => flags.build.push(argument.clone()),
            "short" => flags.test.push("-test.short".into()),
            "cpu" | "parallel" => {
                if let Some(value) = value_of(value) {
                    flags.test.push(format!("-test.{name}={value}"));
                }
            }
            _ => {}
        }
    }
    flags
}

/// The type checker's helper, built from the embedded source into the
/// project's `.supercov`, once per version of it.
const HELPER_SOURCE: &str = include_str!("../runtime-assets/go/mutants/main.go");
const HELPER_MODULE: &str = include_str!("../runtime-assets/go/mutants/go.mod");

fn helper(root: &Path, go: &str) -> Result<PathBuf, String> {
    let digest = crate::stable_ids::digest(&[HELPER_SOURCE, HELPER_MODULE]);
    let directory = root.join(".supercov/cache/go-mutants").join(&digest[..16]);
    let binary = directory.join(if cfg!(windows) {
        "supercov-mutants.exe"
    } else {
        "supercov-mutants"
    });
    if binary.is_file() {
        return Ok(binary);
    }
    let source = directory.join("src");
    crate::fault_fs::create_dir_all(&source).map_err(|error| error.to_string())?;
    crate::fault_fs::write(source.join("main.go"), HELPER_SOURCE).map_err(|e| e.to_string())?;
    crate::fault_fs::write(source.join("go.mod"), HELPER_MODULE).map_err(|e| e.to_string())?;
    let output = Command::new(go)
        .args(["build", "-o"])
        .arg(&binary)
        .arg(".")
        .current_dir(&source)
        .env("GOWORK", "off")
        .env("GOFLAGS", "-mod=mod")
        .env("CGO_ENABLED", "0")
        .launch_output()
        .map_err(|error| format!("go build: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "could not build the Go type-check helper: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(binary)
}

/// The helper's answer for one mutant.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Checked {
    status: String,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    facts: SiteFacts,
}

#[derive(Deserialize)]
struct CheckAnswer {
    mutants: Vec<Checked>,
}

/// Each file's mutants the pass runs: the file's list (every mutant, its
/// token and replacement) and the indices the plan runs.
struct FileMutants {
    source: String,
    tokens: Vec<crate::go_mutants::ListedToken>,
}

/// The pass's own record of what it did, written as the summary line.
#[derive(Default)]
struct Profile {
    switched: usize,
    unplaceable: usize,
    unviable: usize,
    solo_builds: usize,
    build_rounds: usize,
    binaries: usize,
    baselines: usize,
    confirmations: usize,
    overturned: usize,
    runs: usize,
}

/// One package's test binary and what its baseline said.
#[derive(Debug, Clone, Default)]
struct Package {
    binary: PathBuf,
    /// Tests that passed their baseline, with how long each took.
    passed: HashMap<String, u64>,
    /// The baseline ran every test of the package: its candidates' baseline
    /// failed alone and passed whole.
    whole: bool,
    /// How long the baseline took.
    baseline_ms: u64,
    /// The package's binary did not build.
    broken: Option<String>,
}

/// What one run of a test binary showed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunOutcome {
    passed: bool,
    timed_out: bool,
    hit_limit: bool,
    /// The first test reported failed, its top-level name.
    failed: Option<String>,
    /// Top-level tests in the order they started.
    started: Vec<String>,
    /// Each top-level test that passed, and its duration.
    durations: HashMap<String, u64>,
    elapsed_ms: u64,
}

/// Reads `go test -v` output for what ran and how it ended.
fn read_output(text: &str) -> (Option<String>, Vec<String>, HashMap<String, u64>) {
    let mut failed = None;
    let mut started = Vec::new();
    let mut durations = HashMap::new();
    for line in text.lines() {
        let line = line.trim_start();
        if let Some(name) = line.strip_prefix("=== RUN") {
            let name = name.trim();
            if !name.contains('/') && !started.iter().any(|seen| seen == name) {
                started.push(name.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("--- FAIL:") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            let top = name.split('/').next().unwrap_or(name);
            if failed.is_none() && !top.is_empty() {
                failed = Some(top.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("--- PASS:") {
            let mut words = rest.split_whitespace();
            let name = words.next().unwrap_or_default();
            if !name.contains('/') {
                let seconds = words
                    .next()
                    .and_then(|time| time.strip_prefix('('))
                    .and_then(|time| time.strip_suffix("s)"))
                    .and_then(|time| time.parse::<f64>().ok())
                    .unwrap_or(0.0);
                durations.insert(name.to_owned(), (seconds * 1000.0).round() as u64);
            }
        }
    }
    (failed, started, durations)
}

/// `-test.run` matching exactly `names`.
fn anchored(names: &[String]) -> String {
    format!("^(?:{})$", names.join("|"))
}

struct Pass<'a> {
    request: &'a GoStrongRequest<'a>,
    flags: GoFlags,
    workspace: PathBuf,
    bins: PathBuf,
    scratch: PathBuf,
    profile: Mutex<Profile>,
    results: Mutex<std::fs::File>,
    confirmed: Mutex<HashMap<(String, String), bool>>,
}

impl Pass<'_> {
    /// Whether a process of `mutant` passed its hit limit: the runtime's
    /// line beside the results names it.
    fn timed_out(&self, mutant: &str) -> bool {
        let path = crate::strong_driver::StrongRun {
            plan: self.request.plan.to_owned(),
            results: self.request.results.to_owned(),
        }
        .timeouts_path();
        std::fs::read_to_string(path).is_ok_and(|text| {
            text.lines()
                .filter_map(|line| serde_json::from_str::<Value>(line).ok())
                .any(|record| record["mutant"] == mutant)
        })
    }

    fn record(&self, id: &str, verdict: &Verdict) {
        let line = json!({
            "id": id,
            "status": verdict.status,
            "killedBy": verdict.killed_by,
            "tests": verdict.tests,
            "durationMs": verdict.duration_ms,
            "reason": verdict.reason,
        });
        let mut file = self.results.lock().expect("results lock");
        let _ = writeln!(file, "{line}");
        let _ = file.flush();
    }

    /// Runs `binary` in package directory `directory` with `-test.run` over
    /// `names` (every test where `None`), `mutant` active, for at most
    /// `limit`.
    fn run_binary(
        &self,
        binary: &Path,
        directory: &str,
        names: Option<&[String]>,
        mutant: Option<&str>,
        limit: Duration,
    ) -> RunOutcome {
        let started = Instant::now();
        let mut command = Command::new(binary);
        command
            .current_dir(self.workspace.join(directory))
            .arg("-test.v")
            .arg("-test.count=1")
            .arg("-test.paniconexit0")
            .arg(format!("-test.timeout={}ms", limit.as_millis() + 60_000))
            .args(&self.flags.test)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(crate::strong_driver::STRONG_RESULTS, self.request.results)
            .env(crate::strong_driver::MUTANT_HIT_LIMIT, hit_limit())
            .env_remove(ACTIVE_MUTANT_ENV);
        if mutant.is_some() {
            command.arg("-test.failfast");
        }
        if let Some(names) = names {
            command.arg(format!("-test.run={}", anchored(names)));
        }
        if let Some(mutant) = mutant {
            command.env(ACTIVE_MUTANT_ENV, mutant);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        let mut child = match command.launch() {
            Ok(child) => child,
            Err(error) => {
                return RunOutcome {
                    passed: false,
                    timed_out: false,
                    hit_limit: false,
                    failed: Some(format!("<could not start: {error}>")),
                    started: Vec::new(),
                    durations: HashMap::new(),
                    elapsed_ms: 0,
                };
            }
        };
        let readers = [
            child.stdout.take().map(read_all),
            child.stderr.take().map(read_all),
        ];
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Some(status),
                Ok(None) if started.elapsed() > limit => {
                    timed_out = true;
                    kill_tree(&mut child);
                    break child.wait().ok();
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(5)),
                Err(_) => break None,
            }
        };
        let text = readers
            .into_iter()
            .flatten()
            .map(|reader| reader.join().unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n");
        let hit_limit = mutant.is_some_and(|mutant| self.timed_out(mutant));
        let (failed, started_tests, durations) = read_output(&text);
        self.profile.lock().expect("profile").runs += 1;
        RunOutcome {
            passed: !timed_out && status.is_some_and(|status| status.success()),
            timed_out,
            hit_limit,
            failed,
            started: started_tests,
            durations,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
    }
}

/// How many test binaries the pass runs at once: `SUPERCOV_STRONG_WORKERS`,
/// or every CPU.
pub fn workers() -> usize {
    std::env::var("SUPERCOV_STRONG_WORKERS")
        .ok()
        .and_then(|value| value.parse().ok())
        .filter(|workers| *workers > 0)
        .unwrap_or_else(|| std::thread::available_parallelism().map_or(4, std::num::NonZero::get))
}

/// The hit limit a Go mutant's process runs with: `SUPERCOV_MUTANT_HIT_LIMIT`
/// where it is set, else a billion. The contract's million is too few for
/// Go: btree's `TestBTree` evaluates one comparison more than a million
/// times in a test that passes in a quarter of a second, and Gremlins, with
/// no hit limit, calls the mutant there survived. A billion evaluations
/// take seconds, about as long as a run's own time limit.
fn hit_limit() -> String {
    std::env::var(crate::strong_driver::MUTANT_HIT_LIMIT)
        .ok()
        .filter(|limit| limit.parse::<u64>().is_ok_and(|limit| limit > 0))
        .unwrap_or_else(|| GO_HIT_LIMIT.to_string())
}

/// The Go driver's default hit limit.
pub const GO_HIT_LIMIT: u64 = 1_000_000_000;

/// The variable the runtime reads its active mutant from.
const ACTIVE_MUTANT_ENV: &str = crate::strong_driver::ACTIVE_MUTANT;

fn read_all<R: std::io::Read + Send + 'static>(mut reader: R) -> std::thread::JoinHandle<String> {
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let _ = reader.read_to_end(&mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

fn kill_tree(child: &mut std::process::Child) {
    #[cfg(unix)]
    {
        // The test binary's process group: it and whatever it started.
        let group = child.id() as i32;
        // SAFETY: kill(2) with a negative pid signals a process group; the
        // group is the child's own, made for it at spawn.
        unsafe {
            libc::kill(-group, libc::SIGKILL);
        }
    }
    let _ = child.kill();
}

/// Copies the project into `destination`, leaving out what is not the
/// project's (`.git`, `.supercov`).
fn copy_project(source: &Path, destination: &Path) -> Result<(), String> {
    crate::fault_fs::create_dir_all(destination).map_err(|e| e.to_string())?;
    for entry in std::fs::read_dir(source).map_err(|e| format!("{}: {e}", source.display()))? {
        let entry = entry.map_err(|e| e.to_string())?;
        let name = entry.file_name();
        if name == ".git" || name == ".supercov" {
            continue;
        }
        let target = destination.join(&name);
        let kind = entry.file_type().map_err(|e| e.to_string())?;
        if kind.is_dir() {
            copy_project(&entry.path(), &target)?;
        } else if kind.is_symlink() {
            let link = std::fs::read_link(entry.path()).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            std::os::unix::fs::symlink(&link, &target).map_err(|e| e.to_string())?;
            #[cfg(not(unix))]
            {
                let _ = link;
                std::fs::copy(entry.path(), &target).map_err(|e| e.to_string())?;
            }
        } else {
            std::fs::copy(entry.path(), &target)
                .map_err(|e| format!("{}: {e}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// The directory of `file` (`.` for the root), `/`-separated.
fn directory_of(file: &str) -> String {
    file.rsplit_once('/')
        .map_or(".".to_owned(), |(directory, _)| directory.to_owned())
}

/// `./directory` for `go`'s package patterns.
fn pattern(directory: &str) -> String {
    if directory == "." {
        ".".to_owned()
    } else {
        format!("./{directory}")
    }
}

/// The modules the copy holds, by directory, with the import path of the
/// runtime each gets.
fn runtimes(workspace: &Path) -> Result<Vec<(String, String)>, String> {
    let members = crate::go_project::workspace_modules(workspace);
    let directories = if members.is_empty() {
        vec![".".to_owned()]
    } else {
        members.into_iter().collect()
    };
    let mut runtimes = Vec::new();
    for directory in directories {
        let at = workspace.join(&directory);
        let module = crate::go_project::module_path(&at)
            .ok_or_else(|| format!("{}: no module path in go.mod", at.display()))?;
        let runtime = at.join(crate::go_run::RUNTIME_DIRECTORY_NAME);
        crate::fault_fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
        for (name, contents) in crate::go_run::runtime_files() {
            crate::fault_fs::write(runtime.join(name), contents).map_err(|e| e.to_string())?;
        }
        runtimes.push((
            directory,
            format!("{module}/{}", crate::go_run::RUNTIME_DIRECTORY_NAME),
        ));
    }
    runtimes.sort_by_key(|(directory, _)| std::cmp::Reverse(directory.len()));
    Ok(runtimes)
}

fn runtime_for<'r>(runtimes: &'r [(String, String)], file: &str) -> &'r str {
    runtimes
        .iter()
        .find(|(directory, _)| directory == "." || file.starts_with(&format!("{directory}/")))
        .map_or("", |(_, import)| import.as_str())
}

/// Compile errors in `output`: each `file:line:column: message` line, the
/// file relative to `directory` as `go` prints it.
fn compile_errors(output: &str) -> Vec<(String, usize, usize, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (file, rest) = line.split_once(".go:")?;
            let mut parts = rest.splitn(3, ':');
            let line_number = parts.next()?.parse().ok()?;
            let column = parts.next()?.trim().parse().ok()?;
            let message = parts.next().unwrap_or_default().trim().to_owned();
            Some((format!("{}.go", file.trim()), line_number, column, message))
        })
        .collect()
}

/// The byte offset of 1-based `line` and byte `column` in `text`.
fn offset_of(text: &str, line: usize, column: usize) -> Option<usize> {
    let start = if line == 1 {
        0
    } else {
        text.match_indices('\n').nth(line - 2)?.0 + 1
    };
    Some(start + column.saturating_sub(1))
}

/// Runs the plan's undecided mutants and writes a verdict for each to the
/// results. Returns the summary it also writes there.
pub fn run_go_strong(
    request: &GoStrongRequest,
    diagnostics: &mut dyn Write,
) -> Result<Value, String> {
    let started = Instant::now();
    let plan: Plan = serde_json::from_slice(
        &std::fs::read(request.plan).map_err(|e| format!("{}: {e}", request.plan.display()))?,
    )
    .map_err(|e| format!("{}: {e}", request.plan.display()))?;
    let work = request
        .plan
        .parent()
        .ok_or("the plan has no directory")?
        .to_owned();
    let results = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(request.results)
        .map_err(|e| format!("{}: {e}", request.results.display()))?;
    let pass = Pass {
        request,
        flags: go_flags(request.command),
        workspace: work.join("workspace"),
        bins: work.join("bin"),
        scratch: work.join("scratch"),
        profile: Mutex::new(Profile::default()),
        results: Mutex::new(results),
        confirmed: Mutex::new(HashMap::new()),
    };
    for directory in [&pass.bins, &pass.scratch] {
        crate::fault_fs::create_dir_all(directory).map_err(|e| e.to_string())?;
    }
    let to_run = plan
        .mutants
        .iter()
        .filter(|mutant| mutant.decided.is_none())
        .collect::<Vec<_>>();
    if to_run.is_empty() {
        return Ok(summary(&pass, started));
    }

    // Each file's list, read again from the project as the plan read it.
    let mut files = BTreeMap::<String, FileMutants>::new();
    let mut runnable = Vec::new();
    for mutant in &to_run {
        if !files.contains_key(&mutant.file) {
            let source = std::fs::read_to_string(request.root.join(&mutant.file))
                .map_err(|e| format!("{}: {e}", mutant.file))?;
            let tokens = crate::go_mutants::listed_tokens(&source).map_err(|e| e.to_string())?;
            files.insert(mutant.file.clone(), FileMutants { source, tokens });
        }
        let listed = &files[&mutant.file];
        let same = listed
            .tokens
            .get(mutant.index)
            .is_some_and(|(token, mutator, replacement)| {
                crate::go_mutants::mutant_id(
                    &mutant.file,
                    &listed.source,
                    *token,
                    mutator,
                    replacement,
                ) == mutant.id
            });
        if same {
            runnable.push(*mutant);
        } else {
            pass.record(
                &mutant.id,
                &Verdict::of("error", "the file changed since the run: measure it again"),
            );
        }
    }

    // Which mutants compile, and what the type checker says of their sites:
    // every listed mutant of each file, since a constant's switch has to
    // hold the largest constant around it whether or not that one runs.
    let helper = helper(request.root, &pass.flags.program)?;
    let mut asked = Vec::new();
    for (file, listed) in &files {
        for (index, (token, _, replacement)) in listed.tokens.iter().enumerate() {
            asked.push((file.clone(), index, token.0, *replacement));
        }
    }
    let patterns = files
        .keys()
        .map(|file| pattern(&directory_of(file)))
        .collect::<BTreeSet<_>>();
    let check_request = json!({
        "dir": request.root,
        "patterns": patterns,
        "tags": pass.flags.tags,
        "go": pass.flags.program,
        "mutants": asked.iter().map(|(file, _, offset, replacement)| json!({
            "file": file, "offset": offset, "new": replacement,
        })).collect::<Vec<_>>(),
    });
    let checked_at = Instant::now();
    let answer = run_helper(&helper, request.root, &check_request)?;
    let _ = writeln!(
        diagnostics,
        "[supercov] type-checked {} Go mutant(s) in {} ms",
        asked.len(),
        checked_at.elapsed().as_millis()
    );
    let mut checked = HashMap::<(String, usize), Checked>::new();
    for ((file, index, _, _), answer) in asked.into_iter().zip(answer.mutants) {
        checked.insert((file, index), answer);
    }

    // The copy, with the runtime in each module.
    let _ = writeln!(
        diagnostics,
        "[supercov] copying the project for the strong pass"
    );
    if pass.workspace.exists() {
        std::fs::remove_dir_all(&pass.workspace).map_err(|e| e.to_string())?;
    }
    copy_project(request.root, &pass.workspace)?;
    let runtimes = runtimes(&pass.workspace)?;

    // Switches: viable mutants, by file.
    let mut switched_by_file = BTreeMap::<String, Vec<StrongMutant>>::new();
    let mut ids = HashMap::<(String, usize), String>::new();
    let mut live = Vec::new();
    for mutant in &runnable {
        let answer = checked
            .get(&(mutant.file.clone(), mutant.index))
            .cloned()
            .unwrap_or_default();
        if answer.status == "unviable" {
            pass.profile.lock().expect("profile").unviable += 1;
            pass.record(
                &mutant.id,
                &Verdict::of(
                    "unviable",
                    format!("does not compile: {}", answer.error.unwrap_or_default()),
                ),
            );
            continue;
        }
        let (token, mutator, replacement) = files[&mutant.file].tokens[mutant.index];
        switched_by_file
            .entry(mutant.file.clone())
            .or_default()
            .push(StrongMutant {
                index: mutant.index,
                token,
                replacement: replacement.to_owned(),
                mutator: mutator.to_owned(),
            });
        ids.insert((mutant.file.clone(), mutant.index), mutant.id.clone());
        live.push(*mutant);
    }
    let facts_of = |file: &str| -> HashMap<usize, SiteFacts> {
        files[file]
            .tokens
            .iter()
            .enumerate()
            .filter_map(|(index, (token, _, _))| {
                checked
                    .get(&(file.to_owned(), index))
                    .map(|answer| (token.0, answer.facts.clone()))
            })
            .collect()
    };

    // Rendered, built, and rendered again without what the compiler refused.
    let mut refused = HashMap::<String, HashSet<usize>>::new();
    let mut rendered = BTreeMap::<String, StrongFile>::new();
    for round in 0..12 {
        pass.profile.lock().expect("profile").build_rounds = round + 1;
        for (file, mutants) in &switched_by_file {
            let source = &files[file].source;
            let strong = crate::go_instrumenter::strong_rewrite(
                file,
                source,
                mutants,
                &facts_of(file),
                refused.get(file).unwrap_or(&HashSet::new()),
                crate::go_project::language_minor(request.root, file, source),
            )
            .map_err(|e| e.to_string())?;
            let text = strong
                .text
                .replace(RUNTIME_IMPORT, runtime_for(&runtimes, file));
            crate::fault_fs::write(pass.workspace.join(file), &text).map_err(|e| e.to_string())?;
            rendered.insert(file.clone(), StrongFile { text, ..strong });
        }
        let directories = switched_by_file
            .keys()
            .map(|file| pattern(&directory_of(file)))
            .collect::<BTreeSet<_>>();
        let output = Command::new(&pass.flags.program)
            .arg("build")
            .arg("-gcflags=-e")
            .args(&pass.flags.build)
            .args(&directories)
            .current_dir(&pass.workspace)
            .launch_output()
            .map_err(|e| format!("go build: {e}"))?;
        if output.status.success() {
            break;
        }
        let text = String::from_utf8_lossy(&output.stderr).into_owned();
        let mut laid = false;
        for (path, line, column, _) in compile_errors(&text) {
            let relative = path.trim_start_matches("./").to_owned();
            let Some(strong) = rendered.get(&relative) else {
                continue;
            };
            let Some(at) = offset_of(&strong.text, line, column) else {
                continue;
            };
            let refuse = refused.entry(relative.clone()).or_default();
            match crate::go_instrumenter::strong_site_at(strong, at) {
                Some(indices) => {
                    for index in indices {
                        refuse.insert(files[&relative].tokens[*index].0.0);
                    }
                    laid = true;
                }
                None => {
                    // Outside every switch: the file's switches all go.
                    for (_, _, indices) in &strong.sites {
                        for index in indices {
                            refuse.insert(files[&relative].tokens[*index].0.0);
                        }
                    }
                    laid |= !strong.sites.is_empty();
                }
            }
        }
        if !laid {
            return Err(format!(
                "the strong copy does not build, and not at a switch:\n{text}"
            ));
        }
    }
    let mut solo = Vec::new();
    for (file, strong) in &rendered {
        pass.profile.lock().expect("profile").switched += strong.switched.len();
        for (index, reason) in &strong.unplaceable {
            pass.profile.lock().expect("profile").unplaceable += 1;
            solo.push((file.clone(), *index, reason.clone()));
        }
    }
    let _ = writeln!(
        diagnostics,
        "[supercov] switches for {} mutant(s) in {} file(s); {} built alone",
        pass.profile.lock().expect("profile").switched,
        rendered.len(),
        solo.len()
    );

    // Each candidate's package, and the binaries.
    let mut by_package = BTreeMap::<String, BTreeSet<String>>::new();
    for mutant in &live {
        for candidate in &mutant.candidates {
            if let Some((directory, name)) = plan
                .tests
                .get(candidate.test)
                .and_then(|t| split_test(&t.id))
            {
                by_package
                    .entry(directory.to_owned())
                    .or_default()
                    .insert(name.to_owned());
            }
        }
    }
    let packages = build_and_baseline(&pass, &by_package, diagnostics)?;

    // The mutants, a worker each at a time.
    let solo_reasons = solo
        .iter()
        .map(|(file, index, reason)| ((file.clone(), *index), reason.clone()))
        .collect::<HashMap<_, _>>();
    let queue = Mutex::new(
        live.iter()
            .copied()
            .collect::<std::collections::VecDeque<_>>(),
    );
    let workers = request.workers.max(1);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some(mutant) = queue.lock().expect("queue").pop_front() else {
                        break;
                    };
                    let key = (mutant.file.clone(), mutant.index);
                    let verdict = match solo_reasons.get(&key) {
                        Some(reason) => run_solo(&pass, &plan, mutant, &files, &packages, reason),
                        None => run_mutant(&pass, &plan, mutant, &packages, None),
                    };
                    pass.record(&mutant.id, &verdict);
                }
            });
        }
    });
    let summary = summary(&pass, started);
    Ok(summary)
}

fn summary(pass: &Pass, started: Instant) -> Value {
    let profile = pass.profile.lock().expect("profile");
    let summary = json!({
        "summary": true,
        "driver": "go",
        "durationMs": started.elapsed().as_millis() as u64,
        "switched": profile.switched,
        "unplaceable": profile.unplaceable,
        "unviable": profile.unviable,
        "soloBuilds": profile.solo_builds,
        "buildRounds": profile.build_rounds,
        "binaries": profile.binaries,
        "baselines": profile.baselines,
        "confirmations": profile.confirmations,
        "overturned": profile.overturned,
        "runs": profile.runs,
    });
    drop(profile);
    let mut file = pass.results.lock().expect("results lock");
    let _ = writeln!(file, "{summary}");
    summary
}

fn run_helper(helper: &Path, root: &Path, request: &Value) -> Result<CheckAnswer, String> {
    let mut child = Command::new(helper)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .launch()
        .map_err(|e| format!("{}: {e}", helper.display()))?;
    let input = serde_json::to_vec(request).expect("plain data serializes");
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child.wait_with_output().map_err(|e| e.to_string())?;
    let _ = writer.join();
    if !output.status.success() {
        return Err(format!(
            "the Go type-check helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("the Go type-check helper: {e}"))
}

/// Builds each package's test binary and runs its baseline: the package's
/// candidates, with no mutant.
fn build_and_baseline(
    pass: &Pass,
    by_package: &BTreeMap<String, BTreeSet<String>>,
    diagnostics: &mut dyn Write,
) -> Result<BTreeMap<String, Package>, String> {
    let built = Mutex::new(BTreeMap::<String, Package>::new());
    let queue = Mutex::new(by_package.iter().collect::<Vec<_>>());
    let workers = pass.request.workers.clamp(1, 8);
    std::thread::scope(|scope| {
        for _ in 0..workers {
            scope.spawn(|| {
                loop {
                    let Some((directory, names)) = queue.lock().expect("queue").pop() else {
                        break;
                    };
                    let package = build_package(pass, directory, names, None);
                    built
                        .lock()
                        .expect("built")
                        .insert(directory.clone(), package);
                }
            });
        }
    });
    let built = built.into_inner().expect("built");
    let broken = built
        .values()
        .filter(|package| package.broken.is_some())
        .count();
    let _ = writeln!(
        diagnostics,
        "[supercov] built {} test binar{} ({} did not build), baselines done",
        built.len(),
        if built.len() == 1 { "y" } else { "ies" },
        broken
    );
    Ok(built)
}

/// One package's binary (from the copy, or with `overlay`), and its
/// baseline over `names`.
fn build_package(
    pass: &Pass,
    directory: &str,
    names: &BTreeSet<String>,
    overlay: Option<(&Path, &Path)>,
) -> Package {
    let file_name = format!(
        "{}.test",
        crate::stable_ids::digest(&[directory, &format!("{overlay:?}")])
    );
    let binary = match overlay {
        Some((_, bins)) => bins.join(&file_name),
        None => pass.bins.join(&file_name),
    };
    let mut command = Command::new(&pass.flags.program);
    command
        .arg("test")
        .arg("-c")
        // `go vet` would read the switches, not the program.
        .arg("-vet=off")
        .args(&pass.flags.build)
        .arg("-o")
        .arg(&binary);
    if let Some((overlay, _)) = overlay {
        command.arg(format!("-overlay={}", overlay.display()));
    }
    let output = command
        .arg(pattern(directory))
        .current_dir(&pass.workspace)
        .launch_output();
    let mut package = Package {
        binary: binary.clone(),
        ..Package::default()
    };
    match output {
        Ok(output) if output.status.success() && binary.is_file() => {}
        Ok(output) if output.status.success() => {
            package.broken = Some("the package has no test binary".into());
            return package;
        }
        Ok(output) => {
            package.broken = Some(String::from_utf8_lossy(&output.stderr).into_owned());
            return package;
        }
        Err(error) => {
            package.broken = Some(error.to_string());
            return package;
        }
    }
    pass.profile.lock().expect("profile").binaries += 1;
    if overlay.is_some() {
        return package;
    }
    let names = names.iter().cloned().collect::<Vec<_>>();
    let limit = Duration::from_secs(600);
    let baseline = pass.run_binary(&binary, directory, Some(&names), None, limit);
    pass.profile.lock().expect("profile").baselines += 1;
    package.baseline_ms = baseline.elapsed_ms;
    package.passed = baseline.durations.clone();
    if !baseline.passed {
        // Alone the candidates fail: a test may need one before it. Whole,
        // the package ran in the measured run.
        let whole = pass.run_binary(&binary, directory, None, None, limit);
        pass.profile.lock().expect("profile").baselines += 1;
        if whole.passed {
            package.whole = true;
            package.baseline_ms = whole.elapsed_ms;
            package.passed = whole.durations;
        }
    }
    package
}

/// How long a mutant's run of a package may take: three times its
/// baseline and ten seconds, twice what Gremlins' own coefficient gives a
/// package's whole run.
fn limit_for(package: &Package) -> Duration {
    Duration::from_millis(package.baseline_ms.saturating_mul(3) + 10_000)
}

/// A mutant's candidate tests, by package in the order its cheapest
/// candidate puts each package, that passed their baseline.
fn candidate_packages(
    plan: &Plan,
    mutant: &PlanMutant,
    packages: &BTreeMap<String, Package>,
) -> Vec<(String, Vec<String>)> {
    let mut order = Vec::<(String, Vec<String>)>::new();
    for candidate in &mutant.candidates {
        let Some((directory, name)) = plan
            .tests
            .get(candidate.test)
            .and_then(|t| split_test(&t.id))
        else {
            continue;
        };
        let Some(package) = packages.get(directory) else {
            continue;
        };
        if package.broken.is_none() && !package.passed.contains_key(name) {
            continue;
        }
        match order.iter_mut().find(|(seen, _)| seen == directory) {
            Some((_, names)) => names.push(name.to_owned()),
            None => order.push((directory.to_owned(), vec![name.to_owned()])),
        }
    }
    order
}

fn run_mutant(
    pass: &Pass,
    plan: &Plan,
    mutant: &PlanMutant,
    packages: &BTreeMap<String, Package>,
    solo: Option<&BTreeMap<String, Package>>,
) -> Verdict {
    let started = Instant::now();
    let order = candidate_packages(plan, mutant, packages);
    if order.is_empty() {
        return Verdict::of(
            "survived",
            "no candidate test passed its baseline without the mutant",
        );
    }
    let active = format!("{}#{}", mutant.file, mutant.index);
    let mut tests = 0;
    for (directory, names) in &order {
        let package = &packages[directory];
        if let Some(broken) = &package.broken {
            return Verdict {
                duration_ms: started.elapsed().as_millis() as u64,
                ..Verdict::of(
                    "error",
                    format!(
                        "the test package {directory} did not build: {}",
                        first_line(broken)
                    ),
                )
            };
        }
        let binary = solo
            .and_then(|solo| solo.get(directory))
            .map_or(&package.binary, |built| &built.binary);
        if let Some(Some(broken)) = solo.and_then(|solo| solo.get(directory)).map(|p| &p.broken) {
            return Verdict {
                duration_ms: started.elapsed().as_millis() as u64,
                ..Verdict::of(
                    "error",
                    format!(
                        "the test package {directory} did not build with the mutant: {}",
                        first_line(broken)
                    ),
                )
            };
        }
        let selection = (!package.whole).then_some(names.as_slice());
        let limit = limit_for(package);
        let outcome = pass.run_binary(
            binary,
            directory,
            selection,
            solo.is_none().then_some(active.as_str()),
            limit,
        );
        tests += outcome.started.len() as u64;
        if outcome.hit_limit || outcome.timed_out {
            return Verdict {
                status: "timeout",
                killed_by: outcome.started.last().cloned(),
                tests,
                duration_ms: started.elapsed().as_millis() as u64,
                reason: Some(if outcome.hit_limit {
                    "the mutant passed its hit limit".into()
                } else {
                    format!("the tests ran past {} ms", limit.as_millis())
                }),
            };
        }
        if outcome.passed {
            continue;
        }
        // A failure where the selection is not the baseline's: the tests it
        // ran up to the failing one, again without the mutant.
        let killer = outcome.failed.clone();
        if !package.whole
            && let Some(failed) = &killer
            && !confirmed(pass, directory, package, &outcome, failed)
        {
            pass.profile.lock().expect("profile").overturned += 1;
            let all = package.passed.keys().cloned().collect::<Vec<_>>();
            let again = pass.run_binary(
                binary,
                directory,
                Some(&all),
                solo.is_none().then_some(active.as_str()),
                limit,
            );
            tests += again.started.len() as u64;
            if again.passed {
                continue;
            }
            if again.hit_limit || again.timed_out {
                return Verdict {
                    status: "timeout",
                    killed_by: None,
                    tests,
                    duration_ms: started.elapsed().as_millis() as u64,
                    reason: Some("timed out with every candidate of its package".into()),
                };
            }
            return Verdict {
                status: "killed",
                killed_by: again.failed.map(|name| format!("{directory}::{name}")),
                tests,
                duration_ms: started.elapsed().as_millis() as u64,
                reason: Some("killed with every candidate of its package".into()),
            };
        }
        return Verdict {
            status: "killed",
            killed_by: Some(match &killer {
                Some(name) => format!("{directory}::{name}"),
                None => directory.clone(),
            }),
            tests,
            duration_ms: started.elapsed().as_millis() as u64,
            reason: killer
                .is_none()
                .then(|| "the test binary failed outside any test".to_owned()),
        };
    }
    Verdict {
        status: "survived",
        killed_by: None,
        tests,
        duration_ms: started.elapsed().as_millis() as u64,
        reason: None,
    }
}

/// Whether the failure of `failed` in `outcome` is the mutant's: the
/// tests that ran up to it, run again without the mutant, pass.
fn confirmed(
    pass: &Pass,
    directory: &str,
    package: &Package,
    outcome: &RunOutcome,
    failed: &str,
) -> bool {
    let mut ran = Vec::new();
    for name in &outcome.started {
        ran.push(name.clone());
        if name == failed {
            break;
        }
    }
    let mut sorted = ran.clone();
    sorted.sort();
    let all = {
        let mut all = package.passed.keys().cloned().collect::<Vec<_>>();
        all.sort();
        all
    };
    // The baseline ran these exactly, in the same order: no question.
    if sorted == all {
        return true;
    }
    let key = (directory.to_owned(), sorted.join("|"));
    if let Some(known) = pass.confirmed.lock().expect("confirmed").get(&key) {
        return *known;
    }
    pass.profile.lock().expect("profile").confirmations += 1;
    let without = pass.run_binary(
        &package.binary,
        directory,
        Some(&ran),
        None,
        limit_for(package),
    );
    let kill = without.passed;
    pass.confirmed.lock().expect("confirmed").insert(key, kill);
    kill
}

/// A mutant with no switch: its own build of each package it needs, from
/// the file with only it in place.
fn run_solo(
    pass: &Pass,
    plan: &Plan,
    mutant: &PlanMutant,
    files: &BTreeMap<String, FileMutants>,
    packages: &BTreeMap<String, Package>,
    reason: &str,
) -> Verdict {
    let listed = &files[&mutant.file];
    let (token, _, replacement) = listed.tokens[mutant.index];
    let mutated = format!(
        "{}{replacement}{}",
        &listed.source[..token.0],
        &listed.source[token.1..]
    );
    let directory = pass
        .scratch
        .join(format!("solo-{}", crate::stable_ids::digest(&[&mutant.id])));
    let _ = std::fs::create_dir_all(&directory);
    let replaced = directory.join("mutated.go");
    let overlay = directory.join("overlay.json");
    let written = std::fs::write(&replaced, mutated).and_then(|()| {
        std::fs::write(
            &overlay,
            serde_json::to_vec(&json!({
                "Replace": { pass.workspace.join(&mutant.file).to_string_lossy(): replaced }
            }))
            .expect("plain data serializes"),
        )
    });
    if let Err(error) = written {
        return Verdict::of("error", format!("could not write its build: {error}"));
    }
    let mut built = BTreeMap::new();
    for (package_directory, _) in candidate_packages(plan, mutant, packages) {
        let package = build_package(
            pass,
            &package_directory,
            &BTreeSet::new(),
            Some((&overlay, &directory)),
        );
        pass.profile.lock().expect("profile").solo_builds += 1;
        built.insert(package_directory, package);
    }
    let mut verdict = run_mutant(pass, plan, mutant, packages, Some(&built));
    let note = format!("built alone ({reason})");
    verdict.reason = Some(match verdict.reason {
        Some(said) => format!("{said}; {note}"),
        None => note,
    });
    let _ = std::fs::remove_dir_all(&directory);
    verdict
}

fn first_line(text: &str) -> &str {
    text.lines()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .unwrap_or(text)
        .trim()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flags_are_read_from_the_go_test_command() {
        let command = [
            "go",
            "test",
            "-tags",
            "integration",
            "-race",
            "-short",
            "-count=1",
            "-run",
            "X",
            "./...",
        ]
        .map(String::from);
        let flags = go_flags(&command);
        assert_eq!(flags.build, ["-tags=integration", "-race"]);
        assert_eq!(flags.test, ["-test.short"]);
        assert_eq!(flags.tags, "integration");
        assert_eq!(flags.program, "go");
        assert_eq!(go_flags(&["make".into(), "test".into()]).program, "go");
        assert!(go_flags(&["make".into(), "test".into()]).build.is_empty());
        let flags = go_flags(
            &[
                "/usr/bin/go",
                "test",
                "--tags=a,b",
                "-cpu",
                "2",
                "-mod=vendor",
            ]
            .map(String::from),
        );
        assert_eq!(flags.build, ["-tags=a,b", "-mod=vendor"]);
        assert_eq!(flags.test, ["-test.cpu=2"]);
        assert_eq!(flags.program, "/usr/bin/go");
    }

    #[test]
    fn verbose_output_says_what_ran_and_what_failed() {
        let text = "=== RUN   TestA\n--- PASS: TestA (0.25s)\n=== RUN   TestB\n=== RUN   TestB/sub\n    --- FAIL: TestB/sub (0.00s)\n--- FAIL: TestB (0.01s)\nFAIL\n";
        let (failed, started, durations) = read_output(text);
        assert_eq!(failed.as_deref(), Some("TestB"));
        assert_eq!(started, ["TestA", "TestB"]);
        assert_eq!(durations.get("TestA"), Some(&250));
        assert!(!durations.contains_key("TestB"));
    }

    #[test]
    fn compile_errors_are_read_with_their_positions() {
        let output = "# example.com/x\n./a/b.go:3:14: cannot use x (variable of type int) as string value\nnote: module requires Go 1.22\n";
        assert_eq!(
            compile_errors(output),
            [(
                "./a/b.go".to_owned(),
                3,
                14,
                "cannot use x (variable of type int) as string value".to_owned()
            )]
        );
        assert_eq!(offset_of("ab\ncd\nef", 3, 2), Some(7));
        assert_eq!(offset_of("ab", 1, 1), Some(0));
        assert_eq!(offset_of("ab", 3, 1), None);
    }

    #[test]
    fn tests_are_named_exactly() {
        assert_eq!(
            anchored(&["TestA".into(), "TestB".into()]),
            "^(?:TestA|TestB)$"
        );
        assert_eq!(split_test("a/b::TestX"), Some(("a/b", "TestX")));
        assert_eq!(directory_of("a/b/c.go"), "a/b");
        assert_eq!(directory_of("c.go"), ".");
        assert_eq!(pattern("."), ".");
        assert_eq!(pattern("a/b"), "./a/b");
    }
}
