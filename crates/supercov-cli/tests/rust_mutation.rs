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

    let ran = project.supercov(&["runs", "latest", "mutants", "test", "--json"]);
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
    assert_eq!(divided["killedBy"], "src/lib.rs - double (line 1)");
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
}
