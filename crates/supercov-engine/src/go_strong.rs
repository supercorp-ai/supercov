//! The strong pass for Go (`docs/design/strong-mutation.md`, Go): every
//! mutant a plan runs, in test binaries built once.
//!
//! The pass copies the project, writes every mutant it runs into its file
//! behind a switch (`go_instrumenter::strong_rewrite`), and builds each test
//! package's binary once (`go test -c`). One mutant is active per process
//! (`SUPERCOV_ACTIVE_MUTANT`), so a mutant's run is the binary of each
//! package that holds one of its candidate tests, the package whole, as `go
//! test` runs it, stopping at the first failure (`-test.failfast`).
//!
//! Before anything is built, the module's own type checker says which
//! mutants compile (`runtime/go/mutants`, go/types over the package and its
//! tests, as `go test` compiles them), and `go vet` with the checks `go
//! test` runs says which it refuses: such a mutant is `unviable` and gets no
//! switch. A switch the compiler still refuses is found by building, at the
//! site the error is in, and its mutants are built one each, from the file
//! with only that mutant in place.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::fault_fs as fs;
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
    /// Why the plan took it: `not-exact` and `unattributed` say only that
    /// its evidence is not its own, not that it ran the mutant's code.
    #[serde(default)]
    reason: String,
}

/// A test's package directory and its name, from its id
/// (`<package directory>::<name>`).
fn split_test(id: &str) -> Option<(&str, &str)> {
    id.rsplit_once("::")
}

/// `launch_output`, with the child registered while it runs: a signal that
/// stops Supercov stops it too.
trait LaunchRegistered {
    fn launch_registered(&mut self) -> std::io::Result<std::process::Output>;
}

impl LaunchRegistered for Command {
    #[track_caller]
    fn launch_registered(&mut self) -> std::io::Result<std::process::Output> {
        let child = self
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .launch()?;
        let _registered = crate::child_signal_guard::register(&child);
        child.wait_with_output()
    }
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
    /// The command turns `go test`'s vet off (`-vet=off`).
    pub vet_off: bool,
}

/// The flags `command` gives `go test` that change what is built or how
/// tests run: `-tags`, `-race`, `-mod`, `-trimpath`, `-ldflags`, `-gcflags`,
/// `-asmflags`, `-asan`, `-msan`, `-vet`, and `-short`, `-cpu`, `-parallel`
/// for the binary. The rest (`-run`, `-v`, `-count`, `-cover`, `-timeout`)
/// the pass sets itself, or does not need.
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
    let mut arguments = command[test + 2..].iter();
    while let Some(argument) = arguments.next() {
        let flag = argument.trim_start_matches('-');
        let (name, value) = match flag.split_once('=') {
            Some((name, value)) => (name, Some(value.to_owned())),
            None => (flag, None),
        };
        let mut value_of = || value.clone().or_else(|| arguments.next().cloned());
        match name {
            "tags" => {
                let tags = value_of().unwrap_or_default();
                flags.build.push(format!("-tags={tags}"));
                flags.tags = tags;
            }
            "mod" | "ldflags" | "gcflags" | "asmflags" => {
                flags
                    .build
                    .push(format!("-{name}={}", value_of().unwrap_or_default()));
            }
            "vet" => flags.vet_off |= value_of().as_deref() == Some("off"),
            "race" | "trimpath" | "asan" | "msan" => flags.build.push(format!("-{name}")),
            "short" => flags.test.push("-test.short".into()),
            "cpu" | "parallel" => {
                flags
                    .test
                    .push(format!("-test.{name}={}", value_of().unwrap_or_default()));
            }
            _ => {}
        }
    }
    flags
}

/// `flags` with those of each `go test` the command started, as its last
/// run with mutants recorded them (`make test`, a script, gotestsum): each
/// flag once, the first `-tags` kept.
pub fn recorded_flags(flags: GoFlags, invocations: &[Vec<String>]) -> GoFlags {
    invocations.iter().fold(flags, |mut flags, invocation| {
        let command = std::iter::once("go".to_owned())
            .chain(invocation.iter().cloned())
            .collect::<Vec<_>>();
        let seen = go_flags(&command);
        let named = |flag: &str| flag.split('=').next().unwrap_or_default().to_owned();
        let kept = flags
            .build
            .iter()
            .map(|flag| named(flag))
            .collect::<HashSet<_>>();
        flags.build.extend(
            seen.build
                .into_iter()
                .filter(|flag| !kept.contains(&named(flag))),
        );
        let kept = flags.test.clone();
        flags
            .test
            .extend(seen.test.into_iter().filter(|flag| !kept.contains(flag)));
        if flags.tags.is_empty() {
            flags.tags = seen.tags;
        }
        flags.vet_off |= seen.vet_off;
        flags
    })
}

/// `work` on each of `items`, on `workers` threads at once, each taking the
/// next as it is done; on this thread where `workers` is one.
fn on_workers<T: Send, R: Send>(
    workers: usize,
    items: Vec<T>,
    work: impl Fn(T) -> R + Sync,
) -> Vec<R> {
    if workers <= 1 {
        return items.into_iter().map(work).collect();
    }
    let queue = Mutex::new(items.into_iter().collect::<std::collections::VecDeque<_>>());
    let done = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        (0..workers).for_each(|_| {
            scope.spawn(|| {
                while let Some(item) = { queue.lock().expect("queue").pop_front() } {
                    let result = work(item);
                    done.lock().expect("done").push(result);
                }
            });
        });
    });
    done.into_inner().expect("done")
}

/// An I/O failure, as the pass says it.
fn failed(path: &Path) -> impl Fn(std::io::Error) -> String + '_ {
    move |error| format!("{}: {error}", path.display())
}

/// The mutants `go vet` rejects with the checks `go test` runs, of those
/// whose token can change what vet sees (`go_mutants::vet_candidate`), by
/// file and index, with vet's finding. Each is vetted alone: the package
/// with its file replaced by one with only that mutant (`-overlay`). None
/// where the command turns vet off for `go test` (`-vet=off`).
fn vet_rejections(
    pass: &Pass,
    files: &BTreeMap<String, FileMutants>,
    viable: &[&PlanMutant],
) -> HashMap<(String, usize), String> {
    let candidates = viable
        .iter()
        .filter(|mutant| {
            let listed = &files[&mutant.file];
            !pass.flags.vet_off && {
                let (token, _, replacement) = listed.tokens[mutant.index];
                crate::go_mutants::vet_candidate(&listed.source, token, replacement)
            }
        })
        .copied()
        .collect::<Vec<_>>();
    let found = on_workers(pass.request.workers, candidates, |mutant| {
        vet_one(pass, files, mutant).map(|finding| ((mutant.file.clone(), mutant.index), finding))
    })
    .into_iter()
    .flatten()
    .collect::<HashMap<_, _>>();
    pass.profile.lock().expect("profile").vetted = found.len();
    found
}

/// The source of `mutant`'s file with only it in place.
fn mutated_source(files: &BTreeMap<String, FileMutants>, mutant: &PlanMutant) -> String {
    let listed = &files[&mutant.file];
    let (token, _, replacement) = listed.tokens[mutant.index];
    format!(
        "{}{replacement}{}",
        &listed.source[..token.0],
        &listed.source[token.1..]
    )
}

/// An overlay for `go` (`-overlay`) that replaces `file` with `contents`,
/// written in `directory`: the overlay's path.
fn write_overlay(directory: &Path, file: &Path, contents: &str) -> Result<PathBuf, String> {
    // fault-site: go-strong.write-overlay.create-dir-all
    fs::create_dir_all(directory).map_err(failed(directory))?;
    let replaced = directory.join("mutated.go");
    let overlay = directory.join("overlay.json");
    let document = json!({ "Replace": { file.to_string_lossy(): replaced } });
    // fault-site: go-strong.write-overlay.write-1
    fs::write(&replaced, contents).map_err(failed(&replaced))?;
    // fault-site: go-strong.write-overlay.write-2
    fs::write(&overlay, document.to_string()).map_err(failed(&overlay))?;
    Ok(overlay)
}

/// `go vet -atomic -bools` of a mutant's package with only that mutant in
/// place: its first finding, or none.
fn vet_one(
    pass: &Pass,
    files: &BTreeMap<String, FileMutants>,
    mutant: &PlanMutant,
) -> Option<String> {
    let directory = pass
        .scratch
        .join(format!("vet-{}", crate::stable_ids::digest(&[&mutant.id])));
    let overlay = write_overlay(
        &directory,
        &pass.request.root.join(&mutant.file),
        &mutated_source(files, mutant),
    )
    .ok()?;
    let output = Command::new(&pass.flags.program)
        .args(["vet", "-atomic", "-bools"])
        .args(&pass.flags.build)
        .arg(format!("-overlay={}", overlay.display()))
        .arg(pattern(&directory_of(&mutant.file)))
        .current_dir(pass.request.root)
        // fault-site: go-strong.vet-one
        .launch_registered()
        .ok()?;
    let text = String::from_utf8_lossy(&output.stderr).into_owned();
    // The overlay's file is reported by its own path: the mutant's.
    compile_errors(&text)
        .into_iter()
        .next()
        .map(|(_, line, column, message)| format!("{}:{line}:{column}: {message}", mutant.file))
}

/// What the type checker says of every operation token of `files` under
/// `root` (an arithmetic, bitwise or assignment operator's), by file and
/// token offset: for a measured run's value probes, which need an
/// operation's type, and only an operation's. `None` where it cannot say
/// (no `go`, a package that does not check). Kept in `.supercov/cache` for
/// the next run of the same sources, and asked only where there is an
/// operation.
pub(crate) fn site_facts(
    root: &Path,
    project: &Path,
    files: &[String],
    tags: &str,
) -> Option<HashMap<String, HashMap<usize, SiteFacts>>> {
    let mut sources = Vec::new();
    let mut asked = Vec::new();
    for file in files {
        let Ok(source) = fs::read_to_string(root.join(file)) else {
            continue;
        };
        let Ok(tokens) = crate::go_mutants::listed_tokens(&source) else {
            continue;
        };
        let mut offsets = BTreeSet::new();
        for (token, _, replacement) in tokens {
            // One question a token: its facts are its expression's.
            if operation(&source[token.0..token.1]) && offsets.insert(token.0) {
                asked.push((file.clone(), token.0, replacement));
            }
        }
        sources.push((file.clone(), source));
    }
    if asked.is_empty() {
        return Some(HashMap::new());
    }
    let key = facts_key(root, &sources, tags);
    // In the project's own `.supercov`, which the next run keeps: `root` is
    // the run's workspace, made afresh.
    let cache = project.join(".supercov/cache/go-facts.json");
    let cached = fs::read(&cache)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CachedFacts>(&bytes).ok())
        .filter(|cached| cached.key == key);
    if let Some(cached) = cached {
        return Some(cached.facts);
    }
    let helper = helper(project, "go").ok()?;
    let patterns = files
        .iter()
        .map(|file| pattern(&directory_of(file)))
        .collect::<BTreeSet<_>>();
    let request = json!({
        "dir": root,
        "patterns": patterns,
        "factsOnly": true,
        "tags": tags,
        "mutants": asked.iter().map(|(file, offset, replacement)| json!({
            "file": file, "offset": offset, "new": replacement,
        })).collect::<Vec<_>>(),
    });
    let answer = run_helper(&helper, root, &request).ok()?;
    let mut facts = HashMap::<String, HashMap<usize, SiteFacts>>::new();
    asked
        .into_iter()
        .zip(answer.mutants)
        .filter(|(_, checked)| checked.status != "unchecked")
        .for_each(|((file, offset, _), checked)| {
            facts.entry(file).or_default().insert(offset, checked.facts);
        });
    // A cache that cannot be written is asked again next time.
    let _ = fs::write(
        &cache,
        serde_json::to_vec(&CachedFacts {
            key,
            facts: facts.clone(),
        })
        .expect("plain data serializes"),
    );
    Some(facts)
}

/// Whether a token is an operation's, whose value probe needs its type.
fn operation(token: &str) -> bool {
    matches!(
        token.trim_end_matches('='),
        "+" | "-" | "*" | "/" | "%" | "&" | "|" | "^" | "&^" | "<<" | ">>"
    )
}

/// The facts of one run's sources, by what they were asked of.
#[derive(Deserialize, serde::Serialize)]
struct CachedFacts {
    key: String,
    facts: HashMap<String, HashMap<usize, SiteFacts>>,
}

/// What the type checker's facts depend on: the helper, the environment
/// that picks the build, the module's requirements and every source of the
/// run. A newer Go toolchain with the same sources keeps them.
fn facts_key(root: &Path, sources: &[(String, String)], tags: &str) -> String {
    let environment = ["GOFLAGS", "GOOS", "GOARCH", "CGO_ENABLED", "GOEXPERIMENT"]
        .map(|name| std::env::var(name).unwrap_or_default());
    let requirements = ["go.mod", "go.sum", "go.work"]
        .map(|name| fs::read_to_string(root.join(name)).unwrap_or_default());
    let mut parts = vec![HELPER_SOURCE, HELPER_MODULE, tags];
    parts.extend(environment.iter().map(String::as_str));
    parts.extend(requirements.iter().map(String::as_str));
    sources.iter().for_each(|(file, source)| {
        parts.push(file);
        parts.push(source);
    });
    crate::stable_ids::digest(&parts)
}

/// The type checker's helper, built from the embedded source into the
/// project's `.supercov`, once per version of it and per toolchain.
const HELPER_SOURCE: &str = include_str!("../runtime-assets/go/mutants/main.go");
const HELPER_MODULE: &str = include_str!("../runtime-assets/go/mutants/go.mod");

/// The helper's executable's name.
#[cfg(windows)]
const HELPER_NAME: &str = "supercov-mutants.exe";
#[cfg(not(windows))]
const HELPER_NAME: &str = "supercov-mutants";

pub(crate) fn helper(root: &Path, go: &str) -> Result<PathBuf, String> {
    let directory = helper_directory(root, go)?;
    let binary = directory.join(HELPER_NAME);
    if binary.is_file() {
        return Ok(binary);
    }
    let source = directory.join("src");
    // fault-site: go-strong.helper.create-dir-all
    fs::create_dir_all(&source).map_err(failed(&source))?;
    // fault-site: go-strong.helper.write-1
    fs::write(source.join("main.go"), HELPER_SOURCE).map_err(failed(&source))?;
    // fault-site: go-strong.helper.write-2
    fs::write(source.join("go.mod"), HELPER_MODULE).map_err(failed(&source))?;
    let output = Command::new(go)
        // Without its symbol table and paths: a third smaller, and the
        // same program.
        .args(["build", "-trimpath", "-ldflags=-s -w", "-o"])
        .arg(&binary)
        .arg(".")
        .current_dir(&source)
        .env("GOWORK", "off")
        .env("GOFLAGS", "-mod=mod")
        .env("CGO_ENABLED", "0")
        // fault-site: go-strong.helper.launch-registered
        .launch_registered()
        .map_err(failed(Path::new(go)))?;
    if !output.status.success() {
        return Err(format!(
            "could not build the Go type-check helper: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(binary)
}

/// Where `go`'s build of the helper is kept. The helper reads the export
/// data `go list -export` writes with the standard library of the toolchain
/// that built it, so a helper another toolchain built (one from before an
/// upgrade, or for another target) is not reused: the directory is the
/// helper's source and the toolchain's version, target and flags.
fn helper_directory(root: &Path, go: &str) -> Result<PathBuf, String> {
    let output = Command::new(go)
        .args(["env", "GOVERSION", "GOOS", "GOARCH", "GOFLAGS"])
        // fault-site: go-strong.helper.toolchain
        .launch_registered()
        .map_err(failed(Path::new(go)))?;
    if !output.status.success() {
        return Err(format!(
            "could not build the Go type-check helper: `{go} env` failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let toolchain = String::from_utf8_lossy(&output.stdout);
    let digest = crate::stable_ids::digest(&[HELPER_SOURCE, HELPER_MODULE, &toolchain]);
    Ok(root.join(".supercov/cache/go-mutants").join(&digest[..16]))
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

/// A file's list as the pass reads it again: every mutant's token and
/// replacement, in its list's order.
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
    vetted: usize,
    solo_builds: usize,
    build_rounds: usize,
    binaries: usize,
    baselines: usize,
    runs: usize,
    retries: usize,
}

/// One package's test binary and what its baseline said.
#[derive(Debug, Clone, Default)]
struct Package {
    binary: PathBuf,
    /// Tests that fail without a mutant: left out of every run.
    skip: Vec<String>,
    /// How long the baseline took.
    baseline_ms: u64,
    /// Why the package cannot run its mutants: its binary did not build, or
    /// its tests do not pass without one.
    broken: Option<String>,
    /// The directories (from the copy's root) of every package its test
    /// binary links: a binary that links no package of a mutant's cannot
    /// run its code.
    links: Option<HashSet<String>>,
}

/// What one run of a test binary showed.
#[derive(Debug, Clone, PartialEq, Eq)]
struct RunOutcome {
    passed: bool,
    timed_out: bool,
    hit_limit: bool,
    /// Every top-level test reported failed, the first first.
    failures: Vec<String>,
    /// How many top-level tests started.
    started: u64,
    elapsed_ms: u64,
}

/// Reads `go test -v` output for the top-level tests that started and those
/// that failed.
fn read_output(text: &str) -> (u64, Vec<String>) {
    let mut started = HashSet::new();
    let mut failed = Vec::<String>::new();
    text.lines().map(str::trim_start).for_each(|line| {
        if let Some(name) = line.strip_prefix("=== RUN") {
            let name = name.trim();
            if !name.contains('/') {
                started.insert(name.to_owned());
            }
        } else if let Some(rest) = line.strip_prefix("--- FAIL:") {
            let name = rest.split_whitespace().next().unwrap_or_default();
            let top = name.split('/').next().unwrap_or_default().to_owned();
            if !failed.contains(&top) {
                failed.push(top);
            }
        }
    });
    (started.len() as u64, failed)
}

/// A `-test.skip` pattern matching exactly `names`.
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
        fs::read_to_string(path).is_ok_and(|text| {
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
        let _ = fs::write_all(&mut file, format!("{line}\n").as_bytes());
    }

    /// Runs `binary` in package directory `directory`, every test but
    /// `skip`, `mutant` active, for at most `limit`.
    fn run_binary(
        &self,
        binary: &Path,
        directory: &str,
        skip: &[String],
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
            .args(
                (!skip.is_empty())
                    .then(|| format!("-test.skip={}", anchored(skip)))
                    .into_iter()
                    .chain(mutant.map(|_| "-test.failfast".to_owned())),
            )
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env(crate::strong_driver::STRONG_RESULTS, self.request.results)
            .env(crate::strong_driver::MUTANT_HIT_LIMIT, hit_limit())
            .env(ACTIVE_MUTANT_ENV, mutant.unwrap_or("-"));
        process_group(&mut command);
        self.profile.lock().expect("profile").runs += 1;
        // fault-site: go-strong.run-binary
        let Ok(mut child) = command.launch() else {
            // A binary that would not start ran nothing, and passed nothing.
            return RunOutcome {
                passed: false,
                timed_out: false,
                hit_limit: false,
                failures: Vec::new(),
                started: 0,
                elapsed_ms: 0,
            };
        };
        // Its own process group, so a timeout stops what its tests started;
        // so a signal to Supercov stops the group too, which a terminal's
        // Ctrl-C, sent to Supercov's group, does not reach.
        let _registered = crate::child_signal_guard::register_group(&child);
        let readers = [
            read_all(child.stdout.take().expect("piped")),
            read_all(child.stderr.take().expect("piped")),
        ];
        let (status, timed_out) = wait_for(&mut child, started, limit);
        let text = readers
            .map(|reader| reader.join().unwrap_or_default())
            .join("\n");
        let (started_tests, failures) = read_output(&text);
        RunOutcome {
            passed: !timed_out && status.is_some_and(|status| status.success()),
            timed_out,
            hit_limit: mutant.is_some_and(|mutant| self.timed_out(mutant)),
            failures,
            started: started_tests,
            elapsed_ms: started.elapsed().as_millis() as u64,
        }
    }
}

/// `child`'s exit, or its process group killed once `limit` has passed
/// since `started`: the status, and whether the limit ended it.
fn wait_for(
    child: &mut std::process::Child,
    started: Instant,
    limit: Duration,
) -> (Option<std::process::ExitStatus>, bool) {
    loop {
        if let Ok(Some(status)) = child.try_wait() {
            return (Some(status), false);
        }
        if started.elapsed() > limit {
            kill_tree(child);
            return (child.wait().ok(), true);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A test binary starts in a process group of its own, so the whole of what
/// it starts can be stopped with it.
#[cfg(unix)]
fn process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn process_group(_command: &mut Command) {}

#[cfg(unix)]
fn kill_tree(child: &mut std::process::Child) {
    // SAFETY: kill(2) with a negative pid signals a process group; the
    // group is the child's own, made for it at spawn.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
}

#[cfg(not(unix))]
fn kill_tree(child: &mut std::process::Child) {
    let _ = child.kill();
}

/// How many test binaries the pass runs at once: `SUPERCOV_STRONG_WORKERS`,
/// or every CPU.
pub fn workers() -> usize {
    workers_from(std::env::var("SUPERCOV_STRONG_WORKERS").ok())
}

/// `workers` from the variable's value: a positive number, or every CPU.
fn workers_from(value: Option<String>) -> usize {
    value
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
        let _ = fs::read_to_end_from(&mut reader, &mut bytes);
        String::from_utf8_lossy(&bytes).into_owned()
    })
}

/// Copies the project into `destination`, leaving out what is not the
/// project's (`.git`, `.supercov`).
fn copy_project(source: &Path, destination: &Path) -> Result<(), String> {
    // fault-site: go-strong.copy-project.create-dir-all
    fs::create_dir_all(destination).map_err(failed(destination))?;
    // fault-site: go-strong.copy-project.read-dir
    fs::read_dir(source)
        .map_err(failed(source))?
        .flatten()
        .filter(|entry| !matches!(entry.file_name().to_str(), Some(".git" | ".supercov")))
        .try_for_each(|entry| {
            let path = entry.path();
            let target = destination.join(entry.file_name());
            // fault-site: go-strong.copy-project.file-type
            let kind = fs::file_type(&entry).map_err(failed(&path))?;
            if kind.is_dir() {
                copy_project(&path, &target)
            } else if kind.is_symlink() {
                copy_link(&path, &target)
            } else {
                // fault-site: go-strong.copy-project.copy
                fs::copy(&path, &target).map(drop).map_err(failed(&path))
            }
        })
}

/// A symbolic link copied as the link it is.
#[cfg(unix)]
fn copy_link(path: &Path, target: &Path) -> Result<(), String> {
    // fault-site: go-strong.copy-link.read-link
    let link = fs::read_link(path).map_err(failed(path))?;
    // fault-site: go-strong.copy-link.symlink
    fs::symlink(&link, target).map_err(failed(target))
}

/// A symbolic link copied as what it points at: Windows makes links only
/// where a developer allowed it.
#[cfg(not(unix))]
fn copy_link(path: &Path, target: &Path) -> Result<(), String> {
    fs::copy(path, target).map(drop).map_err(failed(path))
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
/// runtime each gets: each its own, a `go.work`'s members each theirs.
fn runtimes(workspace: &Path) -> Result<Vec<(String, String)>, String> {
    let members = crate::go_project::workspace_modules(workspace);
    let directories = if members.is_empty() {
        BTreeSet::from([".".to_owned()])
    } else {
        members
    };
    let mut runtimes = directories
        .into_iter()
        .map(|directory| {
            let at = workspace.join(&directory);
            let module = crate::go_project::module_path(&at)
                .ok_or_else(|| format!("{}: no module path in go.mod", at.display()))?;
            let runtime = at.join(crate::go_run::RUNTIME_DIRECTORY_NAME);
            // fault-site: go-strong.runtimes.create-dir-all
            fs::create_dir_all(&runtime).map_err(failed(&runtime))?;
            crate::go_run::runtime_files()
                .into_iter()
                .try_for_each(|(name, contents)| {
                    // fault-site: go-strong.runtimes.write
                    fs::write(runtime.join(name), contents).map_err(failed(&runtime))
                })?;
            Ok((
                directory,
                format!("{module}/{}", crate::go_run::RUNTIME_DIRECTORY_NAME),
            ))
        })
        .collect::<Result<Vec<_>, String>>()?;
    runtimes.sort_by_key(|(directory, _)| std::cmp::Reverse(directory.len()));
    Ok(runtimes)
}

/// The runtime import of the module `file` is in.
fn runtime_for<'r>(runtimes: &'r [(String, String)], file: &str) -> &'r str {
    runtimes
        .iter()
        .find(|(directory, _)| directory == "." || file.starts_with(&format!("{directory}/")))
        .map_or("", |(_, import)| import.as_str())
}

/// Compile errors in `output`: each `file:line:column: message` line.
fn compile_errors(output: &str) -> Vec<(String, usize, usize, String)> {
    output
        .lines()
        .filter_map(|line| {
            let (file, rest) = line.split_once(".go:")?;
            let (line_number, rest) = rest.split_once(':')?;
            let line_number = line_number.parse().ok()?;
            let (column, message) = rest.split_once(':').unwrap_or((rest, ""));
            let column = column.trim().parse().ok()?;
            Some((
                format!("{}.go", file.trim()),
                line_number,
                column,
                message.trim().to_owned(),
            ))
        })
        .collect()
}

/// The byte offset of 1-based `line` and byte `column` in `text`.
fn offset_of(text: &str, line: usize, column: usize) -> Option<usize> {
    let start = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .nth(line.checked_sub(1)?)?;
    Some(start + column.saturating_sub(1))
}

/// Reads the plan, and opens the results to append to.
fn open(request: &GoStrongRequest) -> Result<(Plan, std::fs::File), String> {
    // fault-site: go-strong.open.read
    let bytes = fs::read(request.plan).map_err(failed(request.plan))?;
    let plan = serde_json::from_slice(&bytes).map_err(|error| error.to_string())?;
    let mut options = std::fs::OpenOptions::new();
    options.create(true).append(true);
    // fault-site: go-strong.open.open-with
    let results = fs::open_with(&options, request.results).map_err(failed(request.results))?;
    Ok((plan, results))
}

/// Runs the plan's undecided mutants and writes a verdict for each to the
/// results. Returns the summary it also writes there.
pub fn run_go_strong(
    request: &GoStrongRequest,
    diagnostics: &mut dyn Write,
) -> Result<Value, String> {
    let started = Instant::now();
    // A signal that stops Supercov stops every process the pass started.
    let _guard = crate::child_signal_guard::ChildSignalGuard::install();
    let (plan, results) = open(request)?;
    let work = request.plan.with_file_name("");
    let pass = Pass {
        request,
        flags: recorded_flags(
            go_flags(request.command),
            &crate::go_run::recorded_go_invocations(request.root, request.command),
        ),
        workspace: work.join("workspace"),
        bins: work.join("bin"),
        scratch: work.join("scratch"),
        profile: Mutex::new(Profile::default()),
        results: Mutex::new(results),
    };
    [&pass.bins, &pass.scratch]
        .into_iter()
        // fault-site: go-strong.run-go-strong
        .try_for_each(|directory| fs::create_dir_all(directory).map_err(failed(directory)))?;
    let to_run = plan
        .mutants
        .iter()
        .filter(|mutant| mutant.decided.is_none())
        .collect::<Vec<_>>();
    if !to_run.is_empty() {
        run_mutants(&pass, &plan, &to_run, diagnostics)?;
    }
    Ok(summary(&pass, started))
}

/// Each file of `mutants` as the pass reads it again, and the mutants whose
/// file still lists them where the plan does: the others are `error`.
fn read_files<'p>(
    pass: &Pass,
    mutants: &[&'p PlanMutant],
) -> Result<(BTreeMap<String, FileMutants>, Vec<&'p PlanMutant>), String> {
    let files = mutants
        .iter()
        .map(|mutant| mutant.file.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .map(|file| {
            let path = pass.request.root.join(&file);
            // fault-site: go-strong.read-files
            let source = fs::read_to_string(&path).map_err(failed(&path))?;
            let tokens = crate::go_mutants::listed_tokens(&source).map_err(|e| e.to_string())?;
            Ok((file, FileMutants { source, tokens }))
        })
        .collect::<Result<BTreeMap<_, _>, String>>()?;
    let (same, changed): (Vec<&PlanMutant>, Vec<&PlanMutant>) =
        mutants.iter().partition(|mutant| {
            let listed = &files[&mutant.file];
            listed
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
                })
        });
    changed.iter().for_each(|mutant| {
        pass.record(
            &mutant.id,
            &Verdict::of("error", "the file changed since the run: measure it again"),
        );
    });
    Ok((files, same))
}

/// What the type checker says of every listed mutant of `files`, by file
/// and index: every one, since a constant's switch has to hold the largest
/// constant around it whether or not that one runs.
fn check(
    pass: &Pass,
    files: &BTreeMap<String, FileMutants>,
    diagnostics: &mut dyn Write,
) -> Result<HashMap<(String, usize), Checked>, String> {
    let helper = helper(pass.request.root, &pass.flags.program)?;
    let asked = files
        .iter()
        .flat_map(|(file, listed)| {
            listed
                .tokens
                .iter()
                .enumerate()
                .map(move |(index, (token, _, replacement))| {
                    (file.clone(), index, token.0, *replacement)
                })
        })
        .collect::<Vec<_>>();
    let patterns = files
        .keys()
        .map(|file| pattern(&directory_of(file)))
        .collect::<BTreeSet<_>>();
    let request = json!({
        "dir": pass.request.root,
        "patterns": patterns,
        "tags": pass.flags.tags,
        "go": pass.flags.program,
        "mutants": asked.iter().map(|(file, _, offset, replacement)| json!({
            "file": file, "offset": offset, "new": replacement,
        })).collect::<Vec<_>>(),
    });
    let checked_at = Instant::now();
    let answer = run_helper(&helper, pass.request.root, &request)?;
    let _ = writeln!(
        diagnostics,
        "[supercov] type-checked {} Go mutant(s) in {} ms",
        asked.len(),
        checked_at.elapsed().as_millis()
    );
    Ok(asked
        .into_iter()
        .zip(answer.mutants)
        .map(|((file, index, _, _), answer)| ((file, index), answer))
        .collect())
}

/// The switched files, rendered, written into the copy and built until the
/// compiler takes them: each round leaves out the switches it refused.
fn render(
    pass: &Pass,
    files: &BTreeMap<String, FileMutants>,
    switched: &BTreeMap<String, Vec<StrongMutant>>,
    checked: &HashMap<(String, usize), Checked>,
    runtimes: &[(String, String)],
) -> Result<BTreeMap<String, StrongFile>, String> {
    let facts_of = |file: &str| -> HashMap<usize, SiteFacts> {
        files[file]
            .tokens
            .iter()
            .enumerate()
            .map(|(index, (token, _, _))| {
                let facts = checked
                    .get(&(file.to_owned(), index))
                    .map(|answer| answer.facts.clone())
                    .unwrap_or_default();
                (token.0, facts)
            })
            .collect()
    };
    let directories = switched
        .keys()
        .map(|file| pattern(&directory_of(file)))
        .collect::<BTreeSet<_>>();
    let mut refused = HashMap::<String, HashSet<usize>>::new();
    // At most a round for each switch: each round refuses one at least.
    loop {
        pass.profile.lock().expect("profile").build_rounds += 1;
        let rendered = switched
            .iter()
            .map(|(file, mutants)| {
                let source = &files[file].source;
                let strong = crate::go_instrumenter::strong_rewrite(
                    file,
                    source,
                    mutants,
                    &facts_of(file),
                    refused.get(file).unwrap_or(&HashSet::new()),
                    crate::go_project::language_minor(pass.request.root, file, source),
                )
                .expect("a file the plan listed parses");
                let text = strong
                    .text
                    .replace(RUNTIME_IMPORT, runtime_for(runtimes, file));
                // The import comes before every switch: each moves by as
                // much as it changed, so a compile error is laid at its own.
                let moved = text.len().cast_signed() - strong.text.len().cast_signed();
                let sites = strong
                    .sites
                    .iter()
                    .map(|(start, end, indices)| {
                        (
                            start.saturating_add_signed(moved),
                            end.saturating_add_signed(moved),
                            indices.clone(),
                        )
                    })
                    .collect();
                // A test's copy that does not build outside its switches.
                let text = crate::fault::or(
                    "go-strong-unbuildable-copy",
                    || format!("{text}\nvar _ int = \"\"\n"),
                    || text.clone(),
                );
                let path = pass.workspace.join(file);
                // fault-site: go-strong.render.write
                fs::write(&path, &text).map_err(failed(&path))?;
                Ok((
                    file.clone(),
                    StrongFile {
                        text,
                        sites,
                        ..strong
                    },
                ))
            })
            .collect::<Result<BTreeMap<_, _>, String>>()?;
        let output = Command::new(&pass.flags.program)
            .arg("build")
            .arg("-gcflags=-e")
            .args(&pass.flags.build)
            .args(&directories)
            .current_dir(&pass.workspace)
            // fault-site: go-strong.render.launch-registered
            .launch_registered()
            .map_err(failed(Path::new(&pass.flags.program)))?;
        if output.status.success() {
            return Ok(rendered);
        }
        let text = String::from_utf8_lossy(&output.stderr).into_owned();
        let before = refused.values().map(HashSet::len).sum::<usize>();
        compile_errors(&text)
            .into_iter()
            .map(|(path, line, column, _)| (path.trim_start_matches("./").to_owned(), line, column))
            // An error in a file with no switch refuses none.
            .filter(|(relative, _, _)| rendered.contains_key(relative))
            .map(|(relative, line, column)| {
                let strong = &rendered[&relative];
                let at = offset_of(&strong.text, line, column)
                    .expect("go reports a position inside the file it compiled");
                // Outside every switch: the file's switches all go.
                let indices = crate::go_instrumenter::strong_site_at(strong, at)
                    .map(<[usize]>::to_vec)
                    .unwrap_or_else(|| strong.switched.clone());
                (relative, indices)
            })
            .for_each(|(relative, indices)| {
                let tokens = &files[&relative].tokens;
                refused
                    .entry(relative.clone())
                    .or_default()
                    .extend(indices.iter().map(|index| tokens[*index].0.0));
            });
        if refused.values().map(HashSet::len).sum::<usize>() == before {
            return Err(format!(
                "the strong copy does not build, and not at a switch:\n{text}"
            ));
        }
    }
}

/// Runs `mutants` (none decided), each against its candidates.
fn run_mutants(
    pass: &Pass,
    plan: &Plan,
    mutants: &[&PlanMutant],
    diagnostics: &mut dyn Write,
) -> Result<(), String> {
    let mut phases = Phases::start();
    let (files, runnable) = read_files(pass, mutants)?;
    let checked = check(pass, &files, diagnostics)?;
    phases.mark("check");
    // A mutant that does not compile, or that `go vet` (which `go test`
    // runs) refuses, is unviable: `go test` fails it before any test runs.
    let (compiled, unviable): (Vec<_>, Vec<_>) = runnable.into_iter().partition(|mutant| {
        checked
            .get(&(mutant.file.clone(), mutant.index))
            .is_none_or(|answer| answer.status != "unviable")
    });
    unviable.iter().for_each(|mutant| {
        let error = checked[&(mutant.file.clone(), mutant.index)].error.clone();
        pass.record(
            &mutant.id,
            &Verdict::of(
                "unviable",
                format!("does not compile: {}", error.unwrap_or_default()),
            ),
        );
    });
    let vetted = vet_rejections(pass, &files, &compiled);
    let (live, rejected): (Vec<_>, Vec<_>) = compiled
        .into_iter()
        .partition(|mutant| !vetted.contains_key(&(mutant.file.clone(), mutant.index)));
    rejected.iter().for_each(|mutant| {
        let finding = &vetted[&(mutant.file.clone(), mutant.index)];
        pass.record(
            &mutant.id,
            &Verdict::of(
                "unviable",
                format!("go vet, which go test runs, rejects it: {finding}"),
            ),
        );
    });
    pass.profile.lock().expect("profile").unviable = unviable.len() + rejected.len();
    phases.mark("vet");

    // The copy, with the runtime in each module, and the switches.
    let _ = writeln!(
        diagnostics,
        "[supercov] copying the project for the strong pass"
    );
    if pass.workspace.exists() {
        // fault-site: go-strong.run-mutants
        fs::remove_dir_all(&pass.workspace).map_err(failed(&pass.workspace))?;
    }
    copy_project(pass.request.root, &pass.workspace)?;
    let runtimes = runtimes(&pass.workspace)?;
    phases.mark("copy");
    let mut switched = BTreeMap::<String, Vec<StrongMutant>>::new();
    live.iter().for_each(|mutant| {
        let (token, mutator, replacement) = files[&mutant.file].tokens[mutant.index];
        switched
            .entry(mutant.file.clone())
            .or_default()
            .push(StrongMutant {
                index: mutant.index,
                token,
                replacement: replacement.to_owned(),
                mutator: mutator.to_owned(),
            });
    });
    let rendered = if switched.is_empty() {
        BTreeMap::new()
    } else {
        render(pass, &files, &switched, &checked, &runtimes)?
    };
    phases.mark("render");
    let solo = rendered
        .iter()
        .flat_map(|(file, strong)| {
            strong
                .unplaceable
                .iter()
                .map(move |(index, reason)| ((file.clone(), *index), reason.clone()))
        })
        .collect::<HashMap<_, _>>();
    {
        let mut profile = pass.profile.lock().expect("profile");
        profile.switched = rendered.values().map(|strong| strong.switched.len()).sum();
        profile.unplaceable = solo.len();
    }
    let _ = writeln!(
        diagnostics,
        "[supercov] switches for {} mutant(s) in {} file(s); {} built alone",
        pass.profile.lock().expect("profile").switched,
        rendered.len(),
        solo.len()
    );

    // Each package that holds a candidate: its binary and its baseline.
    let directories = live
        .iter()
        .flat_map(|mutant| mutant.candidates.iter())
        .filter_map(|candidate| plan.tests.get(candidate.test))
        .filter_map(|test| split_test(&test.id))
        .map(|(directory, _)| directory.to_owned())
        .collect::<BTreeSet<_>>();
    let mut packages = on_workers(
        pass.request.workers.min(8),
        directories.iter().cloned().collect(),
        |directory| {
            let package = build_package(pass, &directory, None);
            (directory, package)
        },
    )
    .into_iter()
    .collect::<BTreeMap<_, _>>();
    let mut links = linked_directories(pass, &directories);
    packages
        .iter_mut()
        .for_each(|(directory, package)| package.links = links.remove(directory));
    phases.mark("binaries");
    let _ = writeln!(
        diagnostics,
        "[supercov] built {} test binaries ({} cannot run their mutants)",
        packages.len(),
        packages
            .values()
            .filter(|package| package.broken.is_some())
            .count()
    );

    // The mutants, a worker each at a time; those past their first limit
    // again, with the full one, in a round of their own.
    let run =
        |mutant: &PlanMutant, first: bool| match solo.get(&(mutant.file.clone(), mutant.index)) {
            Some(reason) => run_solo(pass, plan, mutant, &files, &packages, reason, first),
            None => run_mutant(pass, plan, mutant, &packages, None, first),
        };
    // A mutant past its first limit runs again at once, with the full one,
    // on the same worker: a round of retries after the rest would last a
    // full limit after every other mutant had run.
    on_workers(pass.request.workers, live, |mutant| {
        let mut verdict = run(mutant, true);
        if verdict.status == RETRY {
            pass.profile.lock().expect("profile").retries += 1;
            verdict = run(mutant, false);
        }
        pass.record(&mutant.id, &verdict);
    });
    phases.mark("runs");
    let _ = writeln!(diagnostics, "[supercov] Go pass timings {}", phases.text());
    Ok(())
}

/// How long each phase of a pass took, in the order they ran.
struct Phases {
    since: Instant,
    marks: Vec<(&'static str, Duration)>,
}

impl Phases {
    fn start() -> Self {
        Self {
            since: Instant::now(),
            marks: Vec::new(),
        }
    }

    /// Ends the phase called `name`, and starts the next.
    fn mark(&mut self, name: &'static str) {
        self.marks.push((name, self.since.elapsed()));
        self.since = Instant::now();
    }

    fn text(&self) -> String {
        self.marks
            .iter()
            .map(|(name, took)| format!("{name}={}ms", took.as_millis()))
            .collect::<Vec<_>>()
            .join(" ")
    }
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
        "vetRejected": profile.vetted,
        "soloBuilds": profile.solo_builds,
        "buildRounds": profile.build_rounds,
        "binaries": profile.binaries,
        "baselines": profile.baselines,
        "runs": profile.runs,
        "retries": profile.retries,
    });
    drop(profile);
    let mut file = pass.results.lock().expect("results lock");
    let _ = fs::write_all(&mut file, format!("{summary}\n").as_bytes());
    summary
}

fn run_helper(helper: &Path, root: &Path, request: &Value) -> Result<CheckAnswer, String> {
    let mut child = Command::new(helper)
        .current_dir(root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // fault-site: go-strong.run-helper
        .launch()
        .map_err(failed(helper))?;
    let _registered = crate::child_signal_guard::register(&child);
    let input = serde_json::to_vec(request).expect("plain data serializes");
    let mut stdin = child.stdin.take().expect("piped");
    let writer = std::thread::spawn(move || stdin.write_all(&input));
    let output = child
        .wait_with_output()
        .expect("a process started with its output piped can be waited on");
    let _ = writer.join();
    if !output.status.success() {
        return Err(format!(
            "the Go type-check helper failed: {}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("the Go type-check helper: {error}"))
}

/// One package's binary (from the copy, or with `overlay` and into its
/// directory), and, from the copy, its baseline: the package whole, with no
/// mutant.
fn build_package(pass: &Pass, directory: &str, overlay: Option<&Path>) -> Package {
    let into = overlay.and_then(Path::parent).unwrap_or(&pass.bins);
    let binary = into.join(format!("{}.test", crate::stable_ids::digest(&[directory])));
    let output = Command::new(&pass.flags.program)
        .arg("test")
        .arg("-c")
        // `go vet` would read the switches, not the program.
        .arg("-vet=off")
        .args(&pass.flags.build)
        .args(overlay.map(|overlay| format!("-overlay={}", overlay.display())))
        .arg("-o")
        .arg(&binary)
        .arg(pattern(directory))
        .current_dir(&pass.workspace)
        // fault-site: go-strong.build-package
        .launch_registered();
    let mut package = Package {
        binary: binary.clone(),
        ..Package::default()
    };
    package.broken = match output {
        Ok(output) if output.status.success() && binary.is_file() => None,
        Ok(output) if output.status.success() => Some("the package has no test binary".into()),
        Ok(output) => Some(String::from_utf8_lossy(&output.stderr).into_owned()),
        Err(error) => Some(error.to_string()),
    };
    if package.broken.is_some() {
        return package;
    }
    pass.profile.lock().expect("profile").binaries += 1;
    if overlay.is_some() {
        return package;
    }
    let limit = Duration::from_secs(600);
    let baseline = pass.run_binary(&binary, directory, &[], None, limit);
    pass.profile.lock().expect("profile").baselines += 1;
    package.baseline_ms = baseline.elapsed_ms;
    if baseline.passed {
        return package;
    }
    // A test that fails as written (one the measured run saw fail, or a
    // flaky one) is left out of every run of the package, and the package's
    // baseline is the rest.
    package.skip = baseline.failures;
    let rest = pass.run_binary(&binary, directory, &package.skip, None, limit);
    pass.profile.lock().expect("profile").baselines += 1;
    package.baseline_ms = rest.elapsed_ms;
    if !rest.passed {
        package.broken = Some("its tests do not pass without a mutant".into());
    }
    package
}

/// How long a mutant's run of a package may take: three times its
/// baseline, as Gremlins' own coefficient gives a package's whole run, and
/// three seconds. Only a mutant past its first limit is given it: on the
/// validation modules every one of them hangs, and holds its worker this
/// long.
fn limit_for(package: &Package) -> Duration {
    Duration::from_millis(package.baseline_ms.saturating_mul(3) + 3_000)
}

/// A mutant's first attempt at a package: one and a half times its baseline
/// and a second. A run past it is stopped and tried again, in a round of
/// retries alone, with `limit_for`'s: a mutant that slows its tests down
/// without hanging them keeps its verdict, and one that hangs holds a
/// worker for a second or two rather than for ten.
fn first_limit_for(package: &Package) -> Duration {
    Duration::from_millis(package.baseline_ms.saturating_mul(3) / 2 + 1_000)
}

/// What a first attempt past its short limit says: run again, with the
/// full limit.
const RETRY: &str = "retry";

/// Each test package's binary's linked package directories, from `go list
/// -deps -test`, relative to the copy's root; a package `go list` cannot
/// say has none.
fn linked_directories(
    pass: &Pass,
    directories: &BTreeSet<String>,
) -> BTreeMap<String, HashSet<String>> {
    let output = Command::new(&pass.flags.program)
        .args(["list", "-e", "-deps", "-test"])
        .args(&pass.flags.build)
        .arg("-f={{.ImportPath}}|{{.Dir}}|{{join .Deps \",\"}}")
        .args(directories.iter().map(|directory| pattern(directory)))
        .current_dir(&pass.workspace)
        .launch_registered()
        .map(|output| String::from_utf8_lossy(&output.stdout).into_owned())
        .unwrap_or_default();
    let root = fs::canonicalize(&pass.workspace).unwrap_or_else(|_| pass.workspace.clone());
    let plain = |path: &str| path.split(" [").next().unwrap_or_default().to_owned();
    let listed = output
        .lines()
        .filter_map(|line| listed_package(line, &root))
        .collect::<Vec<_>>();
    let directory_of_import = listed
        .iter()
        .map(|(import, directory, _)| (plain(import), directory.clone()))
        .collect::<HashMap<_, _>>();
    listed
        .iter()
        .filter(|(import, _, _)| import.ends_with(".test"))
        .map(|(_, directory, deps)| {
            let links = deps
                .split(',')
                .filter_map(|dep| directory_of_import.get(&plain(dep)).cloned())
                .chain([directory.clone()])
                .collect::<HashSet<_>>();
            (directory.clone(), links)
        })
        .collect()
}

/// One line of `linked_directories`' listing: the package's import path, its
/// directory relative to `root`, and its dependencies; none for a package
/// outside `root` (the standard library, a module's dependency) or a line
/// that is not one.
fn listed_package(line: &str, root: &Path) -> Option<(String, String, String)> {
    let (import, rest) = line.split_once('|')?;
    let (directory, deps) = rest.split_once('|')?;
    let inside = fs::canonicalize(directory).ok()?;
    let relative = inside
        .strip_prefix(root)
        .ok()?
        .to_string_lossy()
        .replace('\\', "/");
    let relative = if relative.is_empty() {
        ".".to_owned()
    } else {
        relative
    };
    Some((import.to_owned(), relative, deps.to_owned()))
}

/// The packages that hold a mutant's candidates, in the order its cheapest
/// candidate puts them: those whose binary can run its code.
fn candidate_packages(
    plan: &Plan,
    mutant: &PlanMutant,
    packages: &BTreeMap<String, Package>,
) -> Vec<String> {
    let mut order = Vec::<String>::new();
    mutant
        .candidates
        .iter()
        .filter_map(|candidate| {
            let (directory, _) = split_test(&plan.tests[candidate.test].id)
                .expect("a Go test's id names its package");
            let package = &packages[directory];
            // A test the plan took only because its evidence is not its own
            // (an Example, a parallel test), in a binary that does not link
            // the mutant's package: it cannot run the mutant's code.
            let unbounded = matches!(candidate.reason.as_str(), "not-exact" | "unattributed");
            let unlinked = package
                .links
                .as_ref()
                .is_some_and(|links| !links.contains(&directory_of(&mutant.file)));
            (!(unbounded && unlinked)).then(|| directory.to_owned())
        })
        .for_each(|directory| {
            if !order.contains(&directory) {
                order.push(directory);
            }
        });
    order
}

/// The verdict of one run of a mutant's package: none where it passed.
fn judged(outcome: &RunOutcome, directory: &str, first: bool, limit: Duration) -> Option<Verdict> {
    let killer = outcome.failures.first();
    let verdict = |status, killed_by: Option<String>, reason: Option<String>| Verdict {
        status,
        killed_by,
        tests: outcome.started,
        duration_ms: outcome.elapsed_ms,
        reason,
    };
    if outcome.hit_limit {
        return Some(verdict(
            "timeout",
            None,
            Some("the mutant passed its hit limit".into()),
        ));
    }
    match (outcome.timed_out, killer) {
        // A test that failed before the limit settles it.
        (_, Some(name)) => Some(verdict(
            "killed",
            Some(format!("{directory}::{name}")),
            outcome
                .timed_out
                .then(|| "a test failed before the time limit".into()),
        )),
        (true, None) if first => Some(Verdict::of(RETRY, "past its first limit")),
        (true, None) => Some(verdict(
            "timeout",
            None,
            Some(format!("the tests ran past {} ms", limit.as_millis())),
        )),
        (false, None) if outcome.passed => None,
        (false, None) => Some(verdict(
            "killed",
            Some(directory.to_owned()),
            Some("the test binary failed outside any test".into()),
        )),
    }
}

fn run_mutant(
    pass: &Pass,
    plan: &Plan,
    mutant: &PlanMutant,
    packages: &BTreeMap<String, Package>,
    solo: Option<&BTreeMap<String, Package>>,
    first: bool,
) -> Verdict {
    let started = Instant::now();
    let order = candidate_packages(plan, mutant, packages);
    let active = format!("{}#{}", mutant.file, mutant.index);
    let mut tests = 0;
    let decided = order.iter().find_map(|directory| {
        let package = solo.map_or(&packages[directory], |solo| &solo[directory]);
        let skip = &packages[directory].skip;
        if let Some(broken) = &package.broken {
            return Some(Verdict::of(
                "error",
                format!(
                    "the test package {directory} cannot run it: {}",
                    first_line(broken)
                ),
            ));
        }
        let limit = if first {
            first_limit_for(&packages[directory])
        } else {
            limit_for(&packages[directory])
        };
        // The package whole, as `go test` runs it: its tests share the
        // process, so a test after a candidate can fail on what the mutant
        // left behind, and one before it set up what the candidate reads.
        let outcome = pass.run_binary(
            &package.binary,
            directory,
            skip,
            solo.is_none().then_some(active.as_str()),
            limit,
        );
        tests += outcome.started;
        judged(&outcome, directory, first, limit)
    });
    let mut verdict = decided.unwrap_or(Verdict {
        status: "survived",
        killed_by: None,
        tests: 0,
        duration_ms: 0,
        reason: order
            .is_empty()
            .then(|| "no package of its candidates can run its code".to_owned()),
    });
    verdict.tests = tests;
    verdict.duration_ms = started.elapsed().as_millis() as u64;
    verdict
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
    first: bool,
) -> Verdict {
    let directory = pass
        .scratch
        .join(format!("solo-{}", crate::stable_ids::digest(&[&mutant.id])));
    let overlay = match write_overlay(
        &directory,
        &pass.workspace.join(&mutant.file),
        &mutated_source(files, mutant),
    ) {
        Ok(overlay) => overlay,
        Err(error) => return Verdict::of("error", format!("could not write its build: {error}")),
    };
    let built = candidate_packages(plan, mutant, packages)
        .into_iter()
        .map(|package| {
            pass.profile.lock().expect("profile").solo_builds += 1;
            let built = build_package(pass, &package, Some(&overlay));
            (package, built)
        })
        .collect::<BTreeMap<_, _>>();
    let mut verdict = run_mutant(pass, plan, mutant, packages, Some(&built), first);
    let note = format!("built alone ({reason})");
    verdict.reason = Some(
        verdict
            .reason
            .map_or(note.clone(), |said| format!("{said}; {note}")),
    );
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
        // Started by another program, with vet turned off.
        let flags = go_flags(&["env", "A=1", "go", "test", "-vet", "off"].map(String::from));
        assert!(flags.vet_off);
        assert!(!go_flags(&["go", "test", "-vet=all"].map(String::from)).vet_off);
        assert_eq!(
            go_flags(&["go", "test"].map(String::from)),
            GoFlags {
                program: "go".into(),
                ..GoFlags::default()
            }
        );
    }

    #[test]
    fn a_wrapped_command_s_flags_join_the_command_s_own() {
        let own = go_flags(&["go", "test", "-tags", "a", "-short", "-race"].map(String::from));
        let flags = recorded_flags(
            own,
            &[
                [
                    "test",
                    "-tags=b",
                    "-race",
                    "-mod=vendor",
                    "-short",
                    "-cpu=2",
                ]
                .map(String::from)
                .to_vec(),
                ["test", "-vet=off"].map(String::from).to_vec(),
            ],
        );
        assert_eq!(flags.build, ["-tags=a", "-race", "-mod=vendor"]);
        assert_eq!(flags.test, ["-test.short", "-test.cpu=2"]);
        assert_eq!(flags.tags, "a");
        assert!(flags.vet_off);
        // A command with no tags of its own takes the first it started.
        let flags = recorded_flags(
            GoFlags::default(),
            &[["test", "-tags", "b"].map(String::from).to_vec()],
        );
        assert_eq!(flags.tags, "b");
    }

    #[test]
    fn the_workers_are_a_positive_number_or_every_cpu() {
        let every = workers_from(None);
        assert!(every > 0);
        assert_eq!(workers_from(Some("3".into())), 3);
        assert_eq!(workers_from(Some("0".into())), every);
        assert_eq!(workers_from(Some("many".into())), every);
    }

    #[test]
    fn each_module_of_a_workspace_gets_its_own_runtime() {
        let root =
            std::env::temp_dir().join(format!("supercov-go-runtimes-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (file, contents) in [
            ("go.work", "go 1.22\n\nuse (\n\t./a\n\t./a/b\n)\n"),
            ("a/go.mod", "module example.com/a\n"),
            ("a/b/go.mod", "module example.com/b\n"),
        ] {
            std::fs::create_dir_all(root.join(file).parent().unwrap()).unwrap();
            std::fs::write(root.join(file), contents).unwrap();
        }
        let listed = runtimes(&root).unwrap();
        let runtime = crate::go_run::RUNTIME_DIRECTORY_NAME;
        assert_eq!(
            listed,
            [
                ("a/b".to_owned(), format!("example.com/b/{runtime}")),
                ("a".to_owned(), format!("example.com/a/{runtime}")),
            ]
        );
        assert!(root.join("a/b").join(runtime).is_dir());
        assert_eq!(
            runtime_for(&listed, "a/b/c.go"),
            format!("example.com/b/{runtime}")
        );
        assert_eq!(
            runtime_for(&listed, "a/c.go"),
            format!("example.com/a/{runtime}")
        );
        assert_eq!(runtime_for(&listed, "c.go"), "");
        // A member with no module line cannot get one.
        std::fs::write(root.join("a/b/go.mod"), "go 1.22\n").unwrap();
        assert!(runtimes(&root).unwrap_err().contains("no module path"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_listed_package_inside_the_copy_is_named_by_its_directory() {
        let root = std::env::temp_dir().join(format!("supercov-go-listed-{}", std::process::id()));
        std::fs::create_dir_all(root.join("sub")).unwrap();
        let root = std::fs::canonicalize(&root).unwrap();
        let line = |directory: &Path| format!("x/sub|{}|a,b", directory.display());
        assert_eq!(
            listed_package(&line(&root.join("sub")), &root),
            Some(("x/sub".to_owned(), "sub".to_owned(), "a,b".to_owned()))
        );
        assert_eq!(
            listed_package(&line(&root), &root).map(|listed| listed.1),
            Some(".".to_owned())
        );
        // Outside the copy, gone, or not a line of the listing: none.
        assert_eq!(listed_package(&line(root.parent().unwrap()), &root), None);
        assert_eq!(listed_package(&line(&root.join("gone")), &root), None);
        assert_eq!(listed_package("x/sub", &root), None);
        assert_eq!(listed_package("x/sub|dir", &root), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn a_mutant_runs_in_each_package_of_its_candidates_that_can_run_its_code() {
        let plan = Plan {
            tests: vec![
                PlanTest {
                    id: ".::TestA".into(),
                },
                PlanTest {
                    id: "b::TestB".into(),
                },
            ],
            mutants: Vec::new(),
        };
        let candidate = |test, reason: &str| PlanCandidate {
            test,
            reason: reason.into(),
        };
        let mutant = PlanMutant {
            id: "a.go:1:1".into(),
            file: "a.go".into(),
            index: 0,
            decided: None,
            candidates: vec![
                // The root package's binary does not link `a.go`'s.
                candidate(0, "not-exact"),
                candidate(1, "infected"),
                // `b` says nothing of what it links.
                candidate(1, "not-exact"),
                candidate(0, "infected"),
            ],
        };
        let packages = BTreeMap::from([
            (
                ".".to_owned(),
                Package {
                    links: Some(HashSet::from(["x".to_owned()])),
                    ..Package::default()
                },
            ),
            ("b".to_owned(), Package::default()),
        ]);
        assert_eq!(candidate_packages(&plan, &mutant, &packages), ["b", "."]);
    }

    #[test]
    fn each_run_of_a_mutant_s_package_has_its_verdict() {
        let outcome = |passed, timed_out, failures: &[&str]| RunOutcome {
            passed,
            timed_out,
            hit_limit: false,
            failures: failures.iter().map(|name| (*name).to_owned()).collect(),
            started: 1,
            elapsed_ms: 5,
        };
        let limit = Duration::from_secs(2);
        let judge = |outcome: &RunOutcome, first| judged(outcome, "p", first, limit);
        assert_eq!(judge(&outcome(true, false, &[]), true), None);
        let killed = judge(&outcome(false, true, &["TestA"]), true).unwrap();
        assert_eq!(
            (
                killed.status,
                killed.killed_by.as_deref(),
                killed.reason.as_deref()
            ),
            (
                "killed",
                Some("p::TestA"),
                Some("a test failed before the time limit")
            )
        );
        assert_eq!(
            judge(&outcome(false, true, &[]), true).unwrap().status,
            RETRY
        );
        let timeout = judge(&outcome(false, true, &[]), false).unwrap();
        assert_eq!(timeout.status, "timeout");
        assert_eq!(
            timeout.reason.as_deref(),
            Some("the tests ran past 2000 ms")
        );
        let outside = judge(&outcome(false, false, &[]), false).unwrap();
        assert_eq!(
            (outside.status, outside.killed_by.as_deref()),
            ("killed", Some("p"))
        );
        let limited = RunOutcome {
            hit_limit: true,
            ..outcome(false, false, &[])
        };
        assert_eq!(judge(&limited, true).unwrap().status, "timeout");
    }

    /// A project directory whose type-check helper, already built, is
    /// `program`.
    #[cfg(unix)]
    fn with_helper(name: &str, program: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("supercov-go-helper-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let directory = helper_directory(&root, "go").unwrap();
        std::fs::create_dir_all(&directory).unwrap();
        std::os::unix::fs::symlink(program, directory.join(HELPER_NAME)).unwrap();
        root
    }

    #[cfg(unix)]
    #[test]
    fn the_type_checker_s_helper_that_fails_says_so() {
        let root =
            std::env::temp_dir().join(format!("supercov-go-helper-build-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        assert!(
            helper(&root, "false")
                .unwrap_err()
                .contains("could not build")
        );
        let _ = std::fs::remove_dir_all(&root);
        // `sh` reads the request as a script, and fails.
        assert!(
            run_helper(
                Path::new("/bin/sh"),
                Path::new("/"),
                &json!({"mutants": []})
            )
            .is_err_and(|error| error.contains("helper failed"))
        );
        // `cat` answers with the request: not an answer.
        assert!(
            run_helper(
                Path::new("/bin/cat"),
                Path::new("/"),
                &json!({"mutants": 1})
            )
            .is_err_and(|error| error.starts_with("the Go type-check helper: "))
        );
        assert!(
            run_helper(
                Path::new("/bin/cat"),
                Path::new("/"),
                &json!({"mutants": []})
            )
            .is_ok()
        );
    }

    /// A helper is built once per toolchain: the same one keeps it, another
    /// (an upgrade, another target) builds its own.
    #[cfg(unix)]
    #[test]
    fn the_type_checker_s_helper_is_built_again_for_another_toolchain() {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir().join(format!(
            "supercov-go-helper-toolchain-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let builds = root.join("builds");
        // A `go` that says what it is, and builds by writing its `-o` file.
        let go = |name: &str, answer: &str| {
            let path = root.join(name);
            std::fs::write(
                &path,
                format!(
                    "#!/bin/sh\nif [ \"$1\" = env ]; then printf '{answer}'; exit 0; fi\n\
                     echo built >> '{}'\n: > \"$5\"\n",
                    builds.display()
                ),
            )
            .unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path.to_str().unwrap().to_owned()
        };
        let built = || {
            std::fs::read_to_string(&builds)
                .unwrap_or_default()
                .lines()
                .count()
        };
        let old = go("go-old", "go1.22.0\\nlinux\\namd64\\n\\n");
        let first = helper(&root, &old).unwrap();
        assert!(first.is_file());
        assert_eq!(helper(&root, &old).unwrap(), first);
        assert_eq!(built(), 1);
        for (name, answer) in [
            ("go-new", "go1.25.4\\nlinux\\namd64\\n\\n"),
            ("go-arm", "go1.22.0\\nlinux\\narm64\\n\\n"),
            ("go-flags", "go1.22.0\\nlinux\\namd64\\n-tags=x\\n"),
        ] {
            let other = helper(&root, &go(name, answer)).unwrap();
            assert_ne!(other, first, "{name}");
            assert!(other.is_file(), "{name}");
        }
        assert_eq!(built(), 4);
        // The same toolchain under another path keeps its build.
        let again = go("go-again", "go1.22.0\\nlinux\\namd64\\n\\n");
        assert_eq!(helper(&root, &again).unwrap(), first);
        assert_eq!(built(), 4);
        // A toolchain that cannot say what it is builds nothing.
        assert!(
            helper(&root, "false")
                .unwrap_err()
                .contains("`false env` failed")
        );
        // A toolchain that says what it is but cannot build it says why.
        let broken = root.join("go-broken");
        std::fs::write(
            &broken,
            "#!/bin/sh\nif [ \"$1\" = env ]; then printf 'go1.26.0\\nlinux\\namd64\\n\\n'; exit 0; fi\n\
             echo 'no compiler here' >&2\nexit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&broken, std::fs::Permissions::from_mode(0o755)).unwrap();
        let error = helper(&root, broken.to_str().unwrap()).unwrap_err();
        assert!(
            error.starts_with("could not build the Go type-check helper: ")
                && error.contains("no compiler here"),
            "{error}"
        );
        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(unix)]
    #[test]
    fn site_facts_are_none_where_the_type_checker_cannot_say() {
        // A file that is gone or does not parse asks nothing.
        let root = with_helper("asked", "/bin/cat");
        std::fs::write(root.join("bad.go"), "package p\n\nfunc (").unwrap();
        let facts = site_facts(&root, &root, &["gone.go".into(), "bad.go".into()], "").unwrap();
        assert!(facts.is_empty());
        // No file, or a file with no mutant, asks nothing.
        std::fs::write(root.join("empty.go"), "package p\n").unwrap();
        assert!(site_facts(&root, &root, &[], "").unwrap().is_empty());
        assert!(
            site_facts(&root, &root, &["empty.go".into()], "")
                .unwrap()
                .is_empty()
        );
        // Comparisons alone ask nothing: their probes need no type.
        std::fs::write(root.join("c.go"), "package p\n\nvar x = 1 < 2\n").unwrap();
        assert!(
            site_facts(&root, &root, &["c.go".into()], "")
                .unwrap()
                .is_empty()
        );
        // An answer that is not one.
        let source = "package p\n\nvar x = 1 + 2\n";
        std::fs::write(root.join("a.go"), source).unwrap();
        assert_eq!(site_facts(&root, &root, &["a.go".into()], ""), None);
        // What the last run of the same sources was told, it is told again;
        // what a run of other sources was, it is not.
        let facts = HashMap::from([(
            "a.go".to_owned(),
            HashMap::from([(
                source.find('+').unwrap(),
                SiteFacts {
                    integer: true,
                    type_name: "int".into(),
                    ..SiteFacts::default()
                },
            )]),
        )]);
        let cache = |key: String| {
            std::fs::write(
                root.join(".supercov/cache/go-facts.json"),
                serde_json::to_vec(&CachedFacts {
                    key,
                    facts: facts.clone(),
                })
                .unwrap(),
            )
            .unwrap();
        };
        cache(facts_key(&root, &[("a.go".into(), source.into())], ""));
        assert_eq!(
            site_facts(&root, &root, &["a.go".into()], ""),
            Some(facts.clone())
        );
        cache("another".into());
        assert_eq!(site_facts(&root, &root, &["a.go".into()], ""), None);
        let _ = std::fs::remove_dir_all(&root);
        // No helper: its cache cannot be made, `.supercov` being a file.
        let root =
            std::env::temp_dir().join(format!("supercov-go-helper-file-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join(".supercov"), "").unwrap();
        std::fs::write(root.join("a.go"), source).unwrap();
        assert_eq!(site_facts(&root, &root, &["a.go".into()], ""), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn only_an_operation_s_token_needs_its_type() {
        for token in ["+", "-=", "&^", "<<=", ">>", "|"] {
            assert!(operation(token), "{token}");
        }
        for token in ["<", "<=", "==", "!=", "&&", "++", "break"] {
            assert!(!operation(token), "{token}");
        }
    }

    #[test]
    fn a_broken_package_is_named_by_its_first_line_that_says_something() {
        assert_eq!(
            first_line("\n# example.com/x\n  \n./a.go:1:2: no\n"),
            "./a.go:1:2: no"
        );
        assert_eq!(first_line("# only\n"), "# only");
    }

    #[test]
    fn verbose_output_says_what_ran_and_what_failed() {
        let text = "=== RUN   TestA\n--- PASS: TestA (0.25s)\n=== RUN   TestB\n=== RUN   TestB/sub\n    --- FAIL: TestB/sub (0.00s)\n--- FAIL: TestB (0.01s)\nFAIL\n";
        let (started, failed) = read_output(text);
        assert_eq!(failed, ["TestB"]);
        assert_eq!(started, 2);
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
        assert_eq!(offset_of("ab", 0, 1), None);
        // What is not a position is not an error.
        assert_eq!(
            compile_errors("a.go:x:1: m\na.go:3\na.go:3:y: m\nb.go:4:2"),
            [("b.go".to_owned(), 4, 2, String::new())]
        );
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
