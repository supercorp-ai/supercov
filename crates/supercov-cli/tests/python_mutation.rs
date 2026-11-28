//! Mutation testing of a Python run: mutmut's mutants, and a strong pass
//! that forks each one's tests after the runner collected them. Its stops,
//! timeouts, skipped tests, shards and merged runs, as a pass from nothing
//! decides them.
#![cfg(unix)]
mod common;

use std::collections::BTreeMap;

use common::Project;
use serde_json::Value;

/// Each mutant's strong verdict in the run `mutants test` saved into.
fn verdicts(project: &Project, run: &str) -> BTreeMap<String, String> {
    let saved = serde_json::from_str::<Value>(
        &project.read(&format!(".supercov/runs/{run}/strong-mutation.json")),
    )
    .unwrap();
    saved["mutants"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(id, verdict)| (id.clone(), verdict["status"].as_str().unwrap().to_owned()))
        .collect()
}

/// A shop whose tests kill some of its mutants and miss others, in two test
/// modules: the second skips two tests, one where it is declared and one by
/// a condition.
fn shop(project: &Project) {
    project.write(".gitignore", ".supercov\n__pycache__\n");
    project.write("shop/__init__.py", "");
    project.write(
        "shop/cart.py",
        "def shipping(total):\n    if total >= 50:\n        return 0\n    return 5\n\n\ndef label(name):\n    return name.strip().upper()\n",
    );
    project.write("tests/__init__.py", "");
    project.write(
        "tests/test_a.py",
        "import unittest\nfrom shop.cart import shipping\n\n\nclass A(unittest.TestCase):\n    def test_free(self):\n        self.assertEqual(shipping(80), 0)\n\n    def test_paid(self):\n        self.assertEqual(shipping(10), 5)\n",
    );
    project.write(
        "tests/test_b.py",
        "import unittest\nfrom shop.cart import label, shipping\n\n\nclass B(unittest.TestCase):\n    def test_label(self):\n        self.assertEqual(label(' a '), 'A')\n\n    @unittest.skip('later')\n    def test_later(self):\n        self.assertEqual(shipping(50), 1)\n\n    @unittest.skipUnless(False, 'needs credentials')\n    def test_credentials(self):\n        self.assertEqual(label('b'), 'x')\n",
    );
    project.git(&["init", "-q"]);
    project.commit("shop");
}

/// A suite measured in two parts, one module each (`python -m unittest
/// tests.test_a`, `tests.test_b`), merged, and its mutants run in two
/// shards: each shard runs under one command that selects both modules,
/// rather than refusing the two commands or running `supercov merge` as its
/// tests; the skipped tests are recorded `skipped`, and none is counted as a
/// test that did not pass; and the verdicts are those of one pass over the
/// whole suite from nothing.
#[test]
fn a_merged_python_run_s_shards_run_under_one_command_selecting_its_parts() {
    let project = Project::empty("strong-python-merged");
    shop(&project);
    let part = |module: &str| {
        project
            .supercov(&["--", "python3", "-m", "unittest", module])
            .succeeds();
        project.latest()
    };
    let first = part("tests.test_a");
    let second = part("tests.test_b");
    let executions = serde_json::from_str::<Value>(
        &project.read(&format!(".supercov/runs/{second}/test-executions.json")),
    )
    .unwrap();
    let skipped = executions["tests"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|test| test["skipped"] == Value::Bool(true))
        .map(|test| test["test"]["name"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    assert_eq!(skipped.len(), 2, "{executions}");
    assert!(
        skipped.iter().any(|name| name.ends_with("test_later"))
            && skipped
                .iter()
                .any(|name| name.ends_with("test_credentials")),
        "{skipped:?}"
    );
    project.supercov(&["merge", &first, &second]).succeeds();
    let mut last = Value::Null;
    for shard in ["1/2", "2/2"] {
        let ran = project.supercov(&[
            "runs", "latest", "mutants", "test", "--shard", shard, "--json",
        ]);
        let said = ran.stderr();
        let reply = ran.json();
        assert_eq!(ran.code(), 0, "{reply}\n{said}");
        assert!(!said.contains("do not share one test command"), "{said}");
        assert!(!said.contains("did not pass"), "{said}");
        assert_eq!(reply["summary"]["error"], 0, "{reply}\n{said}");
        last = reply;
    }
    let merged = verdicts(&project, last["run"].as_str().unwrap());
    // One pass over the whole suite, from nothing.
    let whole = Project::empty("strong-python-merged-whole");
    shop(&whole);
    let reply = whole
        .supercov(&[
            "mutants", "--json", "--", "python3", "-m", "unittest", "discover", "-s", "tests",
            "-t", ".",
        ])
        .json();
    let fresh = verdicts(&whole, reply["run"].as_str().unwrap());
    assert_eq!(merged, fresh);
    assert!(fresh.values().any(|status| status == "killed"), "{fresh:?}");
    assert!(
        fresh.values().any(|status| status == "survived"),
        "{fresh:?}"
    );
}

/// A pass told to stop judges nothing from then on. Here the mutant's test
/// stands in for the stop: it marks the pass as stopping (`stopped-at`,
/// beside its results, as Supercov does before it signals the command), and
/// its fork ends at once, as one the stop reached before the pass's own
/// process did. Judged, the fork that ended before its tests did made its
/// mutant `killed`; now it has no verdict from this pass.
#[test]
fn a_python_fork_that_ends_once_the_pass_is_stopping_decides_nothing() {
    let project = Project::empty("strong-python-stopping");
    project.write(".gitignore", ".supercov\n__pycache__\n");
    project.write("calc/__init__.py", "");
    project.write("calc/twice.py", "def twice(n):\n    return n * 2\n");
    project.write("tests/__init__.py", "");
    project.write(
        "tests/test_twice.py",
        "import os\nimport unittest\nfrom calc.twice import twice\n\n\nclass Twice(unittest.TestCase):\n    def test_twice(self):\n        results = os.environ.get('SUPERCOV_STRONG_RESULTS')\n        if twice(2) != 4 and results:\n            with open(os.path.join(os.path.dirname(results), 'stopped-at'), 'w') as stream:\n                stream.write('1')\n            os._exit(1)\n        self.assertEqual(twice(2), 4)\n",
    );
    project.git(&["init", "-q"]);
    project.commit("twice");
    project
        .supercov(&[
            "--", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".",
        ])
        .succeeds();
    let ran = project.supercov_with(
        &["runs", "latest", "mutants", "test", "--json"],
        &[("SUPERCOV_STRONG_WORKERS", "1")],
    );
    let listed = project
        .supercov(&["runs", "latest", "mutants", "--json", "--limit", "0"])
        .json();
    let mutants = listed["data"]["mutants"].as_array().unwrap();
    assert!(!mutants.is_empty(), "{listed}");
    for mutant in mutants {
        assert_ne!(
            mutant["strong"]["status"],
            "killed",
            "{mutant}\n{}",
            ran.stderr()
        );
    }
}

/// A mutant's tests that run past their limit beside other forks run again
/// alone, without the mutant first, whose time sets the limit, then with it:
/// a machine busier than when the baseline ran makes no `timeout`. The test
/// here is slow once, the first time it runs with a mutant (it counts its
/// runs beside the pass's results): judged then, the mutant was a `timeout`;
/// run again alone it passes, and the mutant survived.
#[test]
fn a_python_test_past_its_limit_runs_again_alone_before_it_is_a_timeout() {
    let project = Project::empty("strong-python-alone");
    project.write(".gitignore", ".supercov\n__pycache__\n");
    project.write("names/__init__.py", "");
    // No letters: `XX` is the string's only mutant (`lower` and `upper`
    // change nothing).
    project.write("names/tag.py", "def tag():\n    return '1'\n");
    project.write("tests/__init__.py", "");
    project.write(
        "tests/test_tag.py",
        "import os\nimport time\nimport unittest\nfrom names.tag import tag\n\n\nclass Tag(unittest.TestCase):\n    def test_tag(self):\n        results = os.environ.get('SUPERCOV_STRONG_RESULTS')\n        if results:\n            count = os.path.join(os.path.dirname(results), 'tag-runs')\n            runs = len(open(count).read()) if os.path.exists(count) else 0\n            with open(count, 'a') as stream:\n                stream.write('.')\n            # The baseline is the first run, the mutant's the second.\n            if runs == 1:\n                time.sleep(13)\n        self.assertTrue(tag())\n",
    );
    project.git(&["init", "-q"]);
    project.commit("tag");
    project
        .supercov(&[
            "--", "python3", "-m", "unittest", "discover", "-s", "tests", "-t", ".",
        ])
        .succeeds();
    let ran = project.supercov_with(
        &["runs", "latest", "mutants", "test", "--json"],
        &[("SUPERCOV_STRONG_WORKERS", "1")],
    );
    let reply = ran.json();
    let statuses = verdicts(&project, reply["run"].as_str().unwrap());
    assert_eq!(statuses.len(), 1, "{statuses:?}\n{}", ran.stderr());
    assert!(
        statuses.values().all(|status| status == "survived"),
        "{statuses:?}\n{}",
        ran.stderr()
    );
}
