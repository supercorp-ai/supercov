//! The Go strong pass, from a plan, on a module of two packages: every kind
//! of verdict it gives, and each of its own steps failing.
//!
//! Needs `go` on PATH (or Homebrew's) and skips without it. Unix: the
//! copy keeps a symbolic link as one, and a timeout stops a process group.
#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use supercov_engine::go_strong::{GoStrongRequest, run_go_strong};

fn go_binary() -> Option<PathBuf> {
    ["go", "/opt/homebrew/bin/go", "/usr/local/go/bin/go"]
        .into_iter()
        .map(PathBuf::from)
        .find(|path| {
            Command::new(path)
                .arg("version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
}

const CALC: &str = "package calc\n\nimport \"time\"\n\nconst Limit = 2 + 3\n\nfunc Clamp(n int) int {\n\tif n > Limit {\n\t\treturn Limit\n\t}\n\treturn n\n}\n\nfunc Loop(n int) int {\n\ttotal := 0\n\tfor i := 0; i < n; i++ {\n\t\ttotal += i\n\t}\n\treturn total\n}\n\nfunc Join(a, b string) string { return a + b }\n\nfunc Middle(x int) bool { return x != 1 && x != 2 }\n\nfunc Nap(n int) int {\n\tif n > 0 {\n\t\ttime.Sleep(time.Duration(n) * time.Millisecond)\n\t}\n\treturn n\n}\n";

const CALC_TEST: &str = "package calc\n\nimport \"testing\"\n\nfunc TestClamp(t *testing.T) {\n\tif Clamp(6) != 5 || Clamp(1) != 1 {\n\t\tt.Fatal(\"clamp\")\n\t}\n}\n\nfunc TestLoop(t *testing.T) {\n\tif Loop(4) != 6 {\n\t\tt.Fatal(\"loop\")\n\t}\n}\n\nfunc TestJoinAndMiddle(t *testing.T) {\n\tif Join(\"a\", \"b\") != \"ab\" || !Middle(3) || Middle(1) || Nap(1) != 1 {\n\t\tt.Fatal(\"join\")\n\t}\n}\n\nfunc TestAsWritten(t *testing.T) {\n\tt.Fatal(\"fails without a mutant\")\n}\n";

const OTHER: &str = "package other\n\nfunc Twice(n int) int { return n * 2 }\n";

const OTHER_TEST: &str = "package other\n\nimport \"testing\"\n\nfunc TestTwice(t *testing.T) {\n\tif Twice(2) != 4 {\n\t\tt.Fatal(\"twice\")\n\t}\n}\n";

/// A module in a fresh directory under the target directory, with a symlink
/// and a `.git` the copy leaves out.
fn module(name: &str) -> PathBuf {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("go-strong-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for (file, contents) in [
        ("go.mod", "module example.com/calc\n\ngo 1.22\n"),
        ("calc.go", CALC),
        ("calc_test.go", CALC_TEST),
        ("other/other.go", OTHER),
        ("other/other_test.go", OTHER_TEST),
        (".git/HEAD", "ref: refs/heads/main\n"),
        ("testdata/note.txt", "kept\n"),
    ] {
        let path = root.join(file);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, contents).unwrap();
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink("note.txt", root.join("testdata/link.txt")).unwrap();
    root
}

/// The id of `calc.go`'s mutant at the token `token` (its first occurrence
/// after `after`) becoming `replacement`, and its index.
fn mutant(after: &str, token: &str, mutator: &str, replacement: &str) -> (String, usize) {
    let start = CALC.find(after).unwrap();
    let at = start + CALC[start..].find(token).unwrap();
    let listed = supercov_engine::go_mutants::listed_tokens(CALC).unwrap();
    let index = listed
        .iter()
        .position(|(span, listed_mutator, listed_replacement)| {
            span.0 == at && *listed_mutator == mutator && *listed_replacement == replacement
        })
        .unwrap_or_else(|| panic!("no {mutator} {replacement} at {after}"));
    let id = supercov_engine::go_mutants::mutant_id(
        "calc.go",
        CALC,
        listed[index].0,
        mutator,
        replacement,
    );
    (id, index)
}

fn planned(id: &str, index: usize, candidates: &[(usize, &str)]) -> Value {
    json!({
        "id": id, "file": "calc.go", "index": index,
        "candidates": candidates.iter().map(|(test, reason)| json!({"test": test, "reason": reason})).collect::<Vec<_>>(),
    })
}

struct Pass {
    root: PathBuf,
    plan: PathBuf,
    results: PathBuf,
    command: Vec<String>,
}

impl Pass {
    fn new(name: &str, go: &Path, mutants: Vec<Value>) -> Self {
        let root = module(name);
        let work = root.join(".supercov/strong/pass");
        std::fs::create_dir_all(&work).unwrap();
        let plan = work.join("plan.json");
        let tests = [
            ".::TestClamp",
            ".::TestLoop",
            ".::TestJoinAndMiddle",
            ".::TestAsWritten",
            "other::TestTwice",
        ];
        std::fs::write(
            &plan,
            json!({
                "tests": tests.iter().map(|id| json!({"id": id})).collect::<Vec<_>>(),
                "mutants": mutants,
            })
            .to_string(),
        )
        .unwrap();
        Self {
            results: work.join("results.jsonl"),
            plan,
            root,
            command: vec![go.display().to_string(), "test".into(), "./...".into()],
        }
    }

    fn run(&self, faults: &str) -> Result<Value, String> {
        let request = GoStrongRequest {
            root: &self.root,
            command: &self.command,
            plan: &self.plan,
            results: &self.results,
            workers: 1,
        };
        supercov_engine::fault::during(faults, || run_go_strong(&request, &mut Vec::new()))
    }

    fn verdicts(&self) -> std::collections::BTreeMap<String, Value> {
        std::fs::read_to_string(&self.results)
            .unwrap_or_default()
            .lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .filter_map(|record| Some((record["id"].as_str()?.to_owned(), record)))
            .collect()
    }
}

#[test]
fn each_verdict_the_go_pass_gives() {
    let Some(go) = go_binary() else {
        eprintln!("skipping: no Go toolchain found");
        return;
    };
    let every = [
        (0, "infected"),
        (1, "infected"),
        (2, "infected"),
        (3, "infected"),
    ];
    let (boundary, boundary_index) = mutant("if n > Limit", ">", "EqualityOperator", "<=");
    let (constant, constant_index) = mutant("Limit = ", "+", "ArithmeticOperator", "-");
    let (endless, endless_index) = mutant("i++", "++", "UpdateOperator", "--");
    let (strings, strings_index) = mutant("return a + b", "+", "ArithmeticOperator", "-");
    let (vetted, vetted_index) = mutant("x != 1 &&", "&&", "LogicalOperator", "||");
    let (equal, equal_index) = mutant("n > 0", ">", "EqualityOperator", ">=");
    let (untested, untested_index) = mutant("total += i", "+=", "AssignmentOperator", "-=");
    let pass = Pass::new(
        "verdicts",
        &go,
        vec![
            planned(&boundary, boundary_index, &every),
            planned(&constant, constant_index, &every),
            planned(&endless, endless_index, &[(1, "infected")]),
            planned(&strings, strings_index, &every),
            planned(&vetted, vetted_index, &every),
            // Survives: `Nap(1)` sleeps either way.
            planned(&equal, equal_index, &[(2, "infected")]),
            // Its only candidate is in a package that cannot run it.
            planned(&untested, untested_index, &[(4, "not-exact")]),
            // Changed since the run: its id names another mutant.
            json!({"id": "calc.go:1:1-1:2:EqualityOperator:<=", "file": "calc.go", "index": 0, "candidates": []}),
            // Decided by the plan: not run.
            json!({"id": "decided", "file": "calc.go", "index": 0, "decided": "survived"}),
        ],
    );
    let summary = pass.run("").unwrap();
    let verdicts = pass.verdicts();
    let status = |id: &str| verdicts[id]["status"].as_str().unwrap().to_owned();
    assert_eq!(status(&boundary), "killed", "{verdicts:?}");
    assert_eq!(verdicts[&boundary]["killedBy"], ".::TestClamp");
    assert_eq!(status(&constant), "killed", "{verdicts:?}");
    assert!(
        verdicts[&constant]["reason"]
            .as_str()
            .unwrap()
            .contains("built alone")
    );
    assert_eq!(status(&endless), "timeout", "{verdicts:?}");
    assert_eq!(status(&strings), "unviable");
    assert_eq!(status(&vetted), "unviable");
    assert!(
        verdicts[&vetted]["reason"]
            .as_str()
            .unwrap()
            .contains("go vet")
    );
    assert_eq!(status(&equal), "survived");
    assert_eq!(status(&untested), "survived");
    assert_eq!(status("calc.go:1:1-1:2:EqualityOperator:<="), "error");
    assert!(!verdicts.contains_key("decided"));
    assert_eq!(summary["retries"], 1, "{summary}");
    assert_eq!(summary["vetRejected"], 1, "{summary}");
    // A second pass in the same place starts from a fresh copy.
    assert!(pass.run("").is_ok());
}

#[test]
fn a_plan_with_nothing_to_run_writes_only_its_summary() {
    let Some(go) = go_binary() else {
        return;
    };
    let pass = Pass::new(
        "nothing",
        &go,
        vec![json!({"id": "decided", "file": "calc.go", "index": 0, "decided": "no-coverage"})],
    );
    let summary = pass.run("").unwrap();
    assert_eq!(summary["runs"], 0);
    assert!(pass.verdicts().is_empty());
}

/// Each step of the pass that can fail ends it, and says where.
#[test]
fn each_step_of_the_go_pass_that_fails_ends_it() {
    let Some(go) = go_binary() else {
        return;
    };
    let source =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/go_strong.rs"))
            .unwrap();
    // The line a statement is on, by its text: the first line holding its
    // expression, without the binding before it or the `?;` after it, as
    // a measured build's probes leave every line where it was but wrap
    // what is on it.
    let lines = source.lines().collect::<Vec<_>>();
    let core = |snippet: &'static str| {
        let bound = snippet
            .strip_prefix("let ")
            .and_then(|rest| rest.split_once(" = "))
            .map_or(snippet, |(_, value)| value);
        bound
            .strip_suffix("?;")
            .or_else(|| bound.strip_suffix(';'))
            .unwrap_or(bound)
    };
    let line_from = |from: usize, snippet: &'static str| {
        from + lines[from..]
            .iter()
            .position(|line| line.contains(core(snippet)))
            .unwrap_or_else(|| panic!("no line {snippet} after line {from}"))
            + 1
    };
    let line = |snippet: &'static str| line_from(0, snippet);
    let after = |anchor: &'static str, snippet: &'static str| line_from(line(anchor), snippet);
    let (constant, constant_index) = mutant("Limit = ", "+", "ArithmeticOperator", "-");
    let (boundary, boundary_index) = mutant("if n > Limit", ">", "EqualityOperator", "<=");
    let mutants = || {
        vec![
            planned(&constant, constant_index, &[(0, "infected")]),
            planned(&boundary, boundary_index, &[(0, "infected")]),
        ]
    };
    let at = |site: &str, line: usize| format!("{site}#go_strong.rs:{line}");
    for (fault, ends) in [
        (
            at(
                "fs.read",
                line("let bytes = fs::read(request.plan).map_err(failed(request.plan))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.open_with",
                line(
                    "let results = fs::open_with(&options, request.results).map_err(failed(request.results))?;",
                ),
            ),
            true,
        ),
        (
            at(
                "fs.create_dir_all",
                after(
                    "pub fn run_go_strong(",
                    "fs::create_dir_all(directory).map_err(failed(directory))",
                ),
            ),
            true,
        ),
        (
            at(
                "fs.read_to_string",
                line("let source = fs::read_to_string(&path).map_err(failed(&path))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.create_dir_all",
                line("fs::create_dir_all(&source).map_err(failed(&source))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.write",
                line(
                    "fs::write(source.join(\"main.go\"), HELPER_SOURCE).map_err(failed(&source))?;",
                ),
            ),
            true,
        ),
        (
            at(
                "fs.write",
                line(
                    "fs::write(source.join(\"go.mod\"), HELPER_MODULE).map_err(failed(&source))?;",
                ),
            ),
            true,
        ),
        (
            at(
                "spawn",
                after(".env(\"CGO_ENABLED\", \"0\")", ".launch_output()"),
            ),
            true,
        ),
        (at("spawn", after("fn run_helper(", ".launch()")), true),
        (
            at(
                "fs.remove_dir_all",
                line("fs::remove_dir_all(&pass.workspace).map_err(failed(&pass.workspace))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.create_dir_all",
                line("fs::create_dir_all(destination).map_err(failed(destination))?;"),
            ),
            true,
        ),
        (at("fs.read_dir", line("fs::read_dir(source)")), true),
        (
            at(
                "fs.file_type",
                line("let kind = fs::file_type(&entry).map_err(failed(&path))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.read_link",
                line("let link = fs::read_link(path).map_err(failed(path))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.symlink",
                line("fs::symlink(&link, target).map_err(failed(target))"),
            ),
            true,
        ),
        (
            at(
                "fs.copy",
                line("fs::copy(&path, &target).map(drop).map_err(failed(&path))"),
            ),
            true,
        ),
        (
            at(
                "fs.create_dir_all",
                line("fs::create_dir_all(&runtime).map_err(failed(&runtime))?;"),
            ),
            true,
        ),
        (
            at(
                "fs.write",
                line("fs::write(runtime.join(name), contents).map_err(failed(&runtime))"),
            ),
            true,
        ),
        (
            at(
                "fs.write",
                line("fs::write(&path, &text).map_err(failed(&path))?;"),
            ),
            true,
        ),
        (
            at("spawn", after(".arg(\"-gcflags=-e\")", ".launch_output()")),
            true,
        ),
        // A solo build that cannot be written is the mutant's error.
        (
            at(
                "fs.create_dir_all",
                line("fs::create_dir_all(directory).map_err(failed(directory))?;"),
            ),
            false,
        ),
        (
            at(
                "fs.write",
                line("fs::write(&replaced, contents).map_err(failed(&replaced))?;"),
            ),
            false,
        ),
        (
            at(
                "fs.write",
                line("fs::write(&overlay, document.to_string()).map_err(failed(&overlay))?;"),
            ),
            false,
        ),
    ] {
        let pass = Pass::new("faults", &go, mutants());
        // A copy left from before, so its removal is reached.
        std::fs::create_dir_all(pass.plan.with_file_name("workspace")).unwrap();
        let result = pass.run(&fault);
        assert_eq!(result.is_err(), ends, "{fault}: {result:?}");
    }
}
