//! Mutation testing of a Rust run: cargo-mutants' mutants listed from what
//! the run measured, their weak verdicts, and a strong pass that runs each
//! against its candidate tests from one switched build.
mod common;

use common::Project;
use serde_json::Value;

/// A crate whose tests kill some mutants and miss others, with a doctest, an
/// integration test, untested code and a mutant that does not compile.
fn rust_mutants_project(label: &str) -> Project {
    let project = Project::empty(label);
    project.write(
        "Cargo.toml",
        "[package]\nname = \"mini\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        concat!(
            "/// ```\n",
            "/// assert_eq!(mini::double(2), 4);\n",
            "/// ```\n",
            "pub fn double(value: u32) -> u32 {\n",
            "    value * 2\n",
            "}\n",
            "\n",
            "pub fn is_big(value: u32) -> bool {\n",
            "    value > 10\n",
            "}\n",
            "\n",
            "pub fn untested(value: u32) -> u32 {\n",
            "    value + 1\n",
            "}\n",
            "\n",
            "pub fn greet(name: &str) -> String {\n",
            "    let mut out = String::from(\"hi \");\n",
            "    out += name;\n",
            "    out\n",
            "}\n",
            "\n",
            "#[cfg(test)]\n",
            "mod tests {\n",
            "    #[test]\n",
            "    fn big() {\n",
            "        assert!(super::is_big(11));\n",
            "        assert!(!super::is_big(3));\n",
            "    }\n",
            "}\n",
        ),
    );
    project.write(
        "tests/greet.rs",
        "#[test]\nfn greets() {\n    assert_eq!(mini::greet(\"a\"), \"hi a\");\n}\n",
    );
    project.write(".gitignore", "target\n.supercov\n");
    project.git(&["init", "-q"]);
    project
}

fn strong_of(listed: &Value, id: &str) -> Value {
    listed["data"]["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mutant| mutant["id"] == id)
        .unwrap_or_else(|| panic!("no {id} in {listed}"))["strong"]
        .clone()
}

/// The mutants are cargo-mutants', by its ids; `mutants test` kills what a
/// test tells apart, keeps what none does, reports what no test runs as no
/// coverage and what does not compile as unviable, and `mutants` then shows
/// each verdict.
#[test]
fn a_rust_run_lists_cargo_mutants_mutants_and_runs_them_from_one_build() {
    let project = rust_mutants_project("strong-rust");
    project.supercov(&["--", "cargo", "test"]).succeeds();

    let weak = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    assert_eq!(weak["data"]["mutantSet"], "cargo-mutants@27.1.0", "{weak}");
    assert_eq!(weak["data"]["summary"]["mutants"], 17, "{weak}");
    let weak_status = |id: &str| {
        weak["data"]["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mutant| mutant["id"] == id)
            .unwrap_or_else(|| panic!("no {id} in {weak}"))["status"]
            .clone()
    };
    assert_eq!(
        weak_status("src/lib.rs:13:5-13:14:FnValue:0"),
        "not-reached"
    );
    assert_eq!(
        weak_status("src/lib.rs:18:9-18:11:BinaryOperator:-="),
        "unknown"
    );

    // Two tests at a time, as `SUPERCOV_STRONG_JOBS` asks, with no limit on
    // a mutant's evaluations.
    let ran = project.supercov_with(
        &["runs", "latest", "mutants", "test", "--json"],
        &[
            ("SUPERCOV_STRONG_JOBS", "2"),
            ("SUPERCOV_MUTANT_HIT_LIMIT", "0"),
        ],
    );
    let said = ran.stderr();
    let reply = ran.json();
    let summary = &reply["summary"];
    assert_eq!(summary["error"], 0, "{reply}\n{said}");
    assert!(said.contains("switched"), "{said}");
    let listed = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    let status = |id: &str| strong_of(&listed, id)["status"].clone();
    // The doctest gives `double` 2: `*` to `/` fails it, `*` to `+` not.
    let divided = strong_of(&listed, "src/lib.rs:5:11-5:12:BinaryOperator:/");
    assert_eq!(divided["status"], "killed", "{divided}");
    // rustdoc names a doctest by its file's path as the platform writes it.
    let doctest = if cfg!(windows) {
        "src\\lib.rs - double (line 1)"
    } else {
        "src/lib.rs - double (line 1)"
    };
    assert_eq!(divided["killedBy"], doctest);
    assert_eq!(status("src/lib.rs:5:11-5:12:BinaryOperator:+"), "survived");
    // No test gives `is_big` 10.
    assert_eq!(status("src/lib.rs:9:11-9:12:BinaryOperator:>="), "survived");
    let never = strong_of(&listed, "src/lib.rs:9:5-9:15:FnValue:false");
    assert_eq!(never["status"], "killed", "{never}");
    assert_eq!(never["killedBy"], "src/lib.rs::tests::big");
    // An integration test kills a library mutant.
    let emptied = strong_of(&listed, "src/lib.rs:17:5-19:8:FnValue:String::new()");
    assert_eq!(emptied["killedBy"], "tests/greet.rs::greets", "{emptied}");
    assert_eq!(status("src/lib.rs:13:5-13:14:FnValue:0"), "no-coverage");
    // `String -= &str` does not compile.
    let unviable = strong_of(&listed, "src/lib.rs:18:9-18:11:BinaryOperator:-=");
    assert_eq!(unviable["status"], "unviable", "{unviable}");
    assert_eq!(summary["unviable"], 2, "{reply}");
    assert_eq!(summary["noCoverage"], 4, "{reply}");
    assert_eq!(summary["survived"], 2, "{reply}");
    assert_eq!(summary["killed"], 9, "{reply}");

    // A second pass has nothing left to run.
    let again = project
        .supercov(&["runs", "latest", "mutants", "test", "--json"])
        .json();
    assert_eq!(again["summary"]["ran"], 0, "{again}");

    // Measured with the value probes, the run says which mutants its values
    // infect: `is_big` saw 11 and 3, never 10, so `>` to `>=` gives the same
    // value at every evaluation, and `>` to `==` does not.
    project
        .supercov_with(&["--", "cargo", "test"], &[("SUPERCOV_MUTANTS", "1")])
        .succeeds();
    let probed = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    let status = |id: &str| {
        probed["data"]["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .find(|mutant| mutant["id"] == id)
            .unwrap_or_else(|| panic!("no {id} in {probed}"))["status"]
            .clone()
    };
    assert_eq!(
        status("src/lib.rs:9:11-9:12:BinaryOperator:>="),
        "not-infected"
    );
    assert_eq!(status("src/lib.rs:9:11-9:12:BinaryOperator:=="), "infected");
    // `value * 2` with 2: `*` to `+` gives the same, `/` does not.
    assert_eq!(
        status("src/lib.rs:5:11-5:12:BinaryOperator:+"),
        "not-infected"
    );
    assert_eq!(status("src/lib.rs:5:11-5:12:BinaryOperator:/"), "infected");
    // `is_big` returned both: whichever value replaces its body differs.
    assert_eq!(status("src/lib.rs:9:5-9:15:FnValue:true"), "infected");
    assert_eq!(status("src/lib.rs:13:5-13:14:FnValue:0"), "not-reached");

    // A returned `a && b` holds its decision's wrappers, one of them ending
    // where the value does: the value's probe goes around them all, and the
    // build still compiles. Only `false` was returned, so `true` differs.
    // Code the target leaves out, and a constant the compiler evaluates,
    // carry no probe. A returned value whose operator's probe starts, or
    // ends, where it does goes around that probe, and one with a probe
    // touching neither edge (`b * c` in `a + b * c + d`) likewise.
    let both = Project::empty("strong-rust-returned-decision");
    both.write(
        "Cargo.toml",
        "[package]\nname = \"both\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    both.write(
        "src/lib.rs",
        concat!(
            "pub fn both(a: bool, b: bool) -> bool {\n    a && b\n}\n",
            "#[cfg(not(unix))]\npub fn never(a: u8) -> u8 {\n    a + 1\n}\n",
            "pub const K: u8 = 2 + 3;\n",
            "pub fn product_first(a: u8, b: u8, c: u8) -> u8 {\n    a * b + c\n}\n",
            "pub fn product_last(a: u8, b: u8, c: u8) -> u8 {\n    a + b * c\n}\n",
            "pub fn product_inside(a: u8, b: u8, c: u8, d: u8) -> u8 {\n    a + b * c + d\n}\n",
        ),
    );
    both.write(
        "tests/both.rs",
        "#[test]\nfn one_is_not_enough() {\n    assert!(!both::both(true, false));\n    assert!(!both::both(false, true));\n}\n",
    );
    both.write(".gitignore", "target\n.supercov\n");
    both.git(&["init", "-q"]);
    both.supercov_with(&["--", "cargo", "test"], &[("SUPERCOV_MUTANTS", "1")])
        .succeeds();
    let probed = both
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    let returned = probed["data"]["mutants"]
        .as_array()
        .unwrap()
        .iter()
        .find(|mutant| mutant["id"] == "src/lib.rs:2:5-2:11:FnValue:true")
        .unwrap_or_else(|| panic!("no FnValue:true in {probed}"))["status"]
        .clone();
    assert_eq!(returned, "infected", "{probed}");
}

/// Under `cargo nextest run` each test runs through nextest with the
/// project's configuration: a test that needs what a setup script exports
/// passes its baseline and kills the mutants it tells apart.
#[test]
fn a_nextest_rust_run_s_mutants_run_with_its_setup_scripts() {
    let project = Project::empty("strong-rust-nextest");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"flagged\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn scaled(value: u32) -> u32 {\n    value * 3\n}\n",
    );
    project.write(
        "tests/flag.rs",
        "#[test]\nfn scales_with_the_flag() {\n    assert_eq!(std::env::var(\"FLAG\").as_deref(), Ok(\"1\"));\n    assert_eq!(flagged::scaled(2), 6);\n}\n",
    );
    project.write(
        ".config/nextest.toml",
        "experimental = [\"setup-scripts\"]\n\n[scripts.setup.flag]\ncommand = \"sh -c 'echo FLAG=1 >> $NEXTEST_ENV'\"\n\n[[profile.default.scripts]]\nfilter = 'all()'\nsetup = 'flag'\n",
    );
    project.write(".gitignore", "target\n.supercov\n");
    project.git(&["init", "-q"]);
    project
        .supercov(&["--", "cargo", "nextest", "run"])
        .succeeds();
    let ran = project.supercov(&["runs", "latest", "mutants", "test", "--json"]);
    let said = ran.stderr();
    let reply = ran.json();
    assert_eq!(reply["summary"]["error"], 0, "{reply}\n{said}");
    let listed = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    let added = strong_of(&listed, "src/lib.rs:2:11-2:12:BinaryOperator:+");
    assert_eq!(added["status"], "killed", "{added}\n{said}");
    assert_eq!(added["killedBy"], "tests/flag.rs::scales_with_the_flag");
    assert_eq!(reply["summary"]["killed"], 4, "{reply}");
}

/// Interrupted while a test runs a mutant, a pass ends at once with the
/// signal's exit code: the test is stopped with it and says nothing of its
/// mutant, which keeps the candidate it already passed. The next pass runs
/// it against the rest, and reaches the verdict a whole pass does.
#[cfg(unix)]
#[test]
fn an_interrupted_rust_pass_judges_nothing_it_cut_short_and_resumes() {
    let project = Project::empty("strong-rust-interrupted");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"held\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        "pub fn small(v: u32) -> bool {\n    v < 10\n}\n",
    );
    // `b`, with a mutant on, says it started and waits while `hold` is
    // there: only the interruption ends it. It runs in the pass's copy, so
    // both are named by their absolute paths in the project. Without a
    // mutant it is the slower, so `a` runs first.
    let place = |name: &str| format!("{:?}", project.root.join(name).display().to_string());
    project.write(
        "tests/t.rs",
        &format!(
            concat!(
                "#[test]\nfn a() {{\n    assert!(held::small(3));\n}}\n\n",
                "#[test]\nfn b() {{\n",
                "    std::thread::sleep(std::time::Duration::from_secs(2));\n",
                "    if std::env::var_os(\"SUPERCOV_ACTIVE_MUTANT\").is_some() {{\n",
                "        std::fs::write({started}, std::process::id().to_string()).unwrap();\n",
                "        let begun = std::time::Instant::now();\n",
                "        while std::path::Path::new({hold}).exists() && begun.elapsed().as_secs() < 300 {{\n",
                "            std::thread::sleep(std::time::Duration::from_millis(20));\n",
                "        }}\n",
                "    }}\n",
                "    assert!(!held::small(10));\n",
                "}}\n",
            ),
            started = place("started"),
            hold = place("hold"),
        ),
    );
    project.write(".gitignore", "target\n.supercov\nhold\nstarted\n");
    project.git(&["init", "-q"]);
    project.supercov(&["--", "cargo", "test"]).succeeds();

    project.write("hold", "");
    let pass = project
        .supercov_command(&["runs", "latest", "mutants", "test", "--json"])
        .env("SUPERCOV_STRONG_JOBS", "1")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let started = project.root.join("started");
    let begun = std::time::Instant::now();
    while !started.exists() {
        assert!(
            begun.elapsed() < std::time::Duration::from_secs(300),
            "no test started with a mutant"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let test_process: i32 = std::fs::read_to_string(&started).unwrap().parse().unwrap();
    // SAFETY: a signal to the pass alone, as `kill -INT` sends it.
    unsafe { libc::kill(pass.id() as i32, libc::SIGINT) };
    let interrupted = std::time::Instant::now();
    let output = pass.wait_with_output().unwrap();
    let said = String::from_utf8_lossy(&output.stderr);
    assert_eq!(output.status.code(), Some(130), "{said}");
    assert!(
        interrupted.elapsed() < std::time::Duration::from_secs(8),
        "{:?}: {said}",
        interrupted.elapsed()
    );
    // SAFETY: signal 0 only asks whether the process exists.
    assert_ne!(
        unsafe { libc::kill(test_process, 0) },
        0,
        "the test outlived the pass"
    );
    let listed = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    assert!(
        listed["data"]["mutants"]
            .as_array()
            .unwrap()
            .iter()
            .all(|mutant| mutant["strong"].is_null()),
        "{listed}"
    );

    // The rest: `true` runs `b` alone, which kills it as it does `<=`.
    std::fs::remove_file(project.root.join("hold")).unwrap();
    let ran = project.supercov(&["runs", "latest", "mutants", "test", "--json"]);
    let said = ran.stderr();
    let reply = ran.json();
    assert!(
        said.contains(
            "1 mutant(s) resume where earlier passes left them, with 1 of 2 candidates left"
        ),
        "{said}"
    );
    assert_eq!(reply["summary"]["killed"], 5, "{reply}\n{said}");
    let listed = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    for id in [
        "src/lib.rs:2:5-2:11:FnValue:true",
        "src/lib.rs:2:7-2:8:BinaryOperator:<=",
    ] {
        assert_eq!(
            strong_of(&listed, id)["killedBy"],
            "tests/t.rs::b",
            "{listed}"
        );
    }
}

/// A Rust pass carried across commits keeps the verdicts no change could
/// move and reaches, at each commit, the verdicts a pass from nothing does.
/// A test runs from its own function: a function no test's code names is
/// not in reach, one a mutant may lead a test into is (`audit`), and a trait
/// implementation's method is reached by its type's name, which `format!`
/// calls unnamed (`Display::fmt`).
#[test]
fn rust_verdicts_carried_across_commits_are_those_a_fresh_pass_reaches() {
    let project = Project::empty("strong-rust-carried");
    project.write(
        "Cargo.toml",
        "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    );
    project.write(
        "src/lib.rs",
        concat!(
            "pub fn refund(sum: u32) -> u32 {\n    if sum > 1000 {\n        audit(sum)\n    } else {\n        sum\n    }\n}\n\n",
            "pub fn audit(sum: u32) -> u32 {\n    sum\n}\n\n",
            "/// ```\n/// assert_eq!(shop::double(3), 6);\n/// ```\npub fn double(v: u32) -> u32 {\n    v * 2\n}\n\n",
            "pub fn unused(v: u32) -> u32 {\n    v - 1\n}\n\n",
            "pub struct Money(pub u32);\n\n",
            "impl std::fmt::Display for Money {\n    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {\n        write!(f, \"${}\", self.0)\n    }\n}\n\n",
            "pub fn label(m: &Money) -> String {\n    format!(\"{m}\")\n}\n",
        ),
    );
    project.write(
        "tests/t.rs",
        concat!(
            "use shop::*;\n\n",
            "#[test]\nfn refunds() {\n    assert_eq!(refund(20), 20);\n}\n\n",
            "#[test]\nfn doubles() {\n    assert_eq!(double(2), 4);\n}\n\n",
            "#[test]\nfn labels() {\n    assert_eq!(label(&Money(3)), \"$3\");\n}\n",
        ),
    );
    project.write(".gitignore", "target\n.supercov\n.supercov-aside\n");
    project.git(&["init", "-q"]);
    project.commit("fixture");
    let pass = || {
        project
            .supercov(&["mutants", "--json", "--", "cargo", "test"])
            .json()
    };
    let statuses = |run: &str| {
        let saved = serde_json::from_str::<Value>(
            &project.read(&format!(".supercov/runs/{run}/strong-mutation.json")),
        )
        .unwrap();
        saved["mutants"]
            .as_object()
            .unwrap()
            .iter()
            .map(|(id, verdict)| (id.clone(), verdict["status"].as_str().unwrap().to_owned()))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    // A pass from nothing: the store set aside while it runs.
    let store = project.root.join(".supercov");
    let aside = project.root.join(".supercov-aside");
    let fresh = || {
        std::fs::rename(&store, &aside).unwrap();
        let reply = pass();
        let verdicts = statuses(reply["run"].as_str().unwrap());
        std::fs::remove_dir_all(&store).unwrap();
        std::fs::rename(&aside, &store).unwrap();
        (reply, verdicts)
    };
    let first = pass();
    assert_eq!(first["summary"]["error"], 0, "{first}");
    let step = |message: &str, kept: Option<u64>, why: &[&str]| {
        project.commit(message);
        let carried = pass();
        let (whole, verdicts) = fresh();
        assert_eq!(
            statuses(carried["run"].as_str().unwrap()),
            verdicts,
            "{message}: {carried}"
        );
        if let Some(kept) = kept {
            assert_eq!(carried["carried"]["mutants"], kept, "{message}: {carried}");
            assert!(
                carried["summary"]["ran"].as_u64() < whole["summary"]["ran"].as_u64(),
                "{message}: {carried}"
            );
        } else {
            assert!(carried["carried"].is_null(), "{message}: {carried}");
        }
        let reasons = carried["carried"]["notKept"]
            .as_object()
            .map(|reasons| reasons.keys().cloned().collect::<Vec<_>>())
            .unwrap_or_default();
        assert_eq!(reasons, why, "{message}: {carried}");
        carried
    };
    // A comment above every function moves its mutants: each verdict is
    // found by its code.
    project.write(
        "src/lib.rs",
        &format!("// The shop's rules.\n\n{}", project.read("src/lib.rs")),
    );
    // The doctest's name holds its line, which moved: its kill runs again.
    let moved = step("a comment", Some(9), &["killerUnknown"]);
    assert_eq!(moved["summary"]["ran"], 1, "{moved}");
    // One function: its own mutants run again, and nothing else, since no
    // other test's code names it.
    project.edit("src/lib.rs", "    v * 2\n", "    2 * v\n");
    step("double", Some(6), &["fileChanged"]);
    // A function no test runs, which `refund`'s mutants lead `refunds` into:
    // they run again, and the one that sends it there is killed now.
    project.edit(
        "src/lib.rs",
        "pub fn audit(sum: u32) -> u32 {\n    sum\n",
        "pub fn audit(sum: u32) -> u32 {\n    sum + 1\n",
    );
    // The doctest's kill runs again too: a doctest is a comment of the
    // source, and it keeps nothing where code changed.
    let trap = step("audit", Some(6), &["loadsChanged", "testReached"]);
    let now = statuses(trap["run"].as_str().unwrap());
    assert_eq!(
        now["src/lib.rs:4:12-4:13:BinaryOperator:<"], "killed",
        "{trap}"
    );
    // `Display::fmt`, which no code names: `labels` names `Money`.
    project.edit("src/lib.rs", "self.0)", "0 + self.0)");
    step("fmt", Some(6), &["fileChanged", "testReached"]);
    // A function nothing names: every verdict is kept but the doctest's.
    project.edit("src/lib.rs", "    v - 1\n", "    v.wrapping_sub(1)\n");
    let unnamed = step("unused", Some(9), &["testReached"]);
    assert_eq!(unnamed["summary"]["ran"], 1, "{unnamed}");
    // A comment after every function moves no line: every verdict is
    // kept, the doctest's with them, since no code changed.
    project.write(
        "src/lib.rs",
        &format!("{}// The end.\n", project.read("src/lib.rs")),
    );
    let ended = step("a comment at the end", Some(10), &[]);
    assert_eq!(ended["summary"]["ran"], 0, "{ended}");
    // The build's configuration: nothing is kept.
    project.write(
        "Cargo.toml",
        "[package]\nname = \"shop\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[profile.dev]\ndebug = 1\n",
    );
    step("configuration", None, &[]);
}
