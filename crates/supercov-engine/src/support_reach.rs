//! Which tests reached which project files through the processes that served
//! them. A suite's mock servers are processes of their own, started by
//! Playwright's `webServer` and outside what is measured; nothing ties their
//! files to tests, so a change to one counted for every test, and a suite
//! measured again in full after any mock changed. Each Node process under
//! the run notes the project files it loaded and read, and the tests it
//! served (`register.mjs`, `resolve-loader.mjs`, `runtime.mjs`). A process
//! that served requests reaches every file it loaded for each of those
//! tests: a mock keeps state between tests, so its files are judged whole,
//! never by route.

use crate::fault_fs as fs;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::served_tests::ServedTest;

/// The archive entry the reach is kept in.
pub const ARCHIVE_PATH: &str = "support-reach.json";
/// Where each process notes what it loaded, read and served.
pub const DIRECTORY_VARIABLE: &str = "SUPERCOV_SUPPORT_DIR";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupportReach {
    /// Each project file a serving process loaded or read, relative to the
    /// project, and the tests (by id) those processes served.
    pub files: BTreeMap<String, BTreeSet<String>>,
    /// Each of those tests' name, as the run names it.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
}

impl SupportReach {
    /// One reach from several (a merge's runs).
    pub fn extend(&mut self, other: SupportReach) {
        for (file, tests) in other.files {
            self.files.entry(file).or_default().extend(tests);
        }
        self.names.extend(other.names);
    }
}

/// What one process noted.
#[derive(Default)]
struct Process {
    files: BTreeSet<String>,
    served: BTreeSet<String>,
    unscoped: Vec<i64>,
}

/// The reach from the processes' notes in `directory`. `workspace` is the
/// copy the processes ran in; `served` the browser run's tests, with when
/// each ran. None when no process served a request.
pub fn read(directory: &Path, workspace: &Path, served: &[ServedTest]) -> Option<SupportReach> {
    let roots = [
        Some(workspace.to_path_buf()),
        fs::canonicalize(workspace).ok(),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>();
    let relative = |noted: &str| -> Option<String> {
        let path = match noted.strip_prefix("file://") {
            Some(rest) => url_path(rest),
            None => noted.to_owned(),
        };
        let path = Path::new(&path);
        roots.iter().find_map(|root| {
            let relative = path.strip_prefix(root).ok()?;
            let relative = relative.to_string_lossy().replace('\\', "/");
            // Supercov's own files, under the copy's `.supercov/`, and
            // dependencies are no project file.
            (!relative.is_empty()
                && !relative.starts_with(".supercov/")
                && !relative.split('/').any(|part| part == "node_modules"))
            .then_some(relative)
        })
    };
    let mut processes = Vec::new();
    for entry in fs::read_dir(directory).ok()?.flatten() {
        let path = entry.path();
        if path
            .extension()
            .is_none_or(|extension| extension != "jsonl")
        {
            continue;
        }
        let Ok(text) = fs::read_to_string(&path) else {
            continue;
        };
        let mut process = Process::default();
        for line in text.lines() {
            let Ok(record) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            if let Some(noted) = record["loaded"].as_str().or(record["read"].as_str())
                && let Some(file) = relative(noted)
            {
                process.files.insert(file);
            } else if let Some(test) = record["served"].as_str() {
                process.served.insert(test.to_owned());
            } else if let Some(at) = record["unscoped"].as_i64() {
                process.unscoped.push(at);
            }
        }
        processes.push(process);
    }
    let mut reach = SupportReach::default();
    let mut any = false;
    for process in processes {
        // A process no request reached is not a server: a test worker loads
        // the tests' helpers, which their imports already place.
        if process.served.is_empty() && process.unscoped.is_empty() {
            continue;
        }
        any = true;
        let mut tests = process.served;
        // A request that named no test is the test's that was running then,
        // if only one was.
        for at in process.unscoped {
            let mut running = served.iter().filter(|test| {
                test.window
                    .is_some_and(|(start, end)| start <= at && at <= end)
            });
            if let (Some(test), None) = (running.next(), running.next()) {
                tests.insert(test.test_id.clone());
            }
        }
        for test in &tests {
            if let Some(served) = served.iter().find(|served| served.test_id == *test) {
                reach.names.insert(test.clone(), served.name.clone());
            }
        }
        for file in process.files {
            reach
                .files
                .entry(file)
                .or_default()
                .extend(tests.iter().cloned());
        }
    }
    any.then_some(reach)
}

/// A file URL's path, its percent escapes decoded.
fn url_path(rest: &str) -> String {
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    let bytes = rest.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%'
            && index + 2 < bytes.len()
            // Bytes, not `rest[..]`: a `%` before a character of several
            // bytes would cut that character.
            && let Ok(digits) = std::str::from_utf8(&bytes[index + 1..index + 3])
            && let Ok(byte) = u8::from_str_radix(digits, 16)
        {
            decoded.push(byte);
            index += 3;
            continue;
        }
        decoded.push(bytes[index]);
        index += 1;
    }
    let path = String::from_utf8_lossy(&decoded).into_owned();
    // `file:///C:/x` on Windows.
    if path.len() > 2 && path.as_bytes()[0] == b'/' && path.as_bytes()[2] == b':' {
        return path[1..].to_owned();
    }
    path
}

#[cfg(test)]
mod tests {
    use super::*;

    fn served(id: &str, window: (i64, i64)) -> ServedTest {
        ServedTest {
            worker: "playwright:0".into(),
            test_id: id.into(),
            retry: 0,
            name: id.into(),
            title: id.into(),
            file: "tests/a.spec.ts".into(),
            status: "passed".into(),
            expected_status: "passed".into(),
            provenance: Default::default(),
            window: Some(window),
            attempt_id: None,
        }
    }

    #[test]
    fn a_serving_process_reaches_its_files_for_the_tests_it_served() {
        let root = std::env::temp_dir().join(format!(
            "supercov-support-{}-{}",
            std::process::id(),
            line!()
        ));
        let workspace = root.join("copy");
        let directory = root.join("support");
        fs::create_dir_all(&directory).unwrap();
        let mock = format!(
            "file://{}/tests/mocks/stripe%20server.mjs",
            workspace.display()
        );
        let data = workspace.join("tests/mocks/stripe-api/requests.json");
        fs::write(
            directory.join("1-a.jsonl"),
            format!(
                "{}\n{}\n{}\n{}\n",
                serde_json::json!({ "loaded": mock }),
                serde_json::json!({ "read": data.display().to_string() }),
                serde_json::json!({ "served": "t-pay" }),
                // A webhook while only t-refund ran.
                serde_json::json!({ "unscoped": 150 }),
            ),
        )
        .unwrap();
        // A test worker: loads helpers, serves nothing.
        fs::write(
            directory.join("2-b.jsonl"),
            format!("{}\n", serde_json::json!({ "loaded": format!("file://{}/tests/helpers.ts", workspace.display()) })),
        )
        .unwrap();
        let reach = read(
            &directory,
            &workspace,
            &[served("t-pay", (0, 100)), served("t-refund", (120, 200))],
        )
        .unwrap();
        let both = BTreeSet::from(["t-pay".to_owned(), "t-refund".to_owned()]);
        assert_eq!(
            reach.files.get("tests/mocks/stripe server.mjs"),
            Some(&both)
        );
        assert_eq!(
            reach.files.get("tests/mocks/stripe-api/requests.json"),
            Some(&both)
        );
        assert!(!reach.files.contains_key("tests/helpers.ts"));
        assert_eq!(
            reach.names.get("t-refund").map(String::as_str),
            Some("t-refund")
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A file URL decodes its escapes, leaves a malformed or cut-short one
    /// as written, and drops the slash before a Windows drive.
    #[test]
    fn file_url_paths_decode_what_is_an_escape() {
        for (rest, expected) in [
            ("/a%20b", "/a b"),
            ("localhost/a", "/a"),
            ("/a%2", "/a%2"),
            ("/a%", "/a%"),
            ("/a%zz", "/a%zz"),
            ("/a%aé", "/a%aé"),
            ("/C:/x", "C:/x"),
            ("/C", "/C"),
            ("/Cx/y", "/Cx/y"),
            ("C:/x", "C:/x"),
            ("", ""),
        ] {
            assert_eq!(url_path(rest), expected, "{rest}");
        }
    }

    /// Only a project file is noted: the workspace itself, Supercov's own
    /// files and dependencies are not. Other files in the directory, lines
    /// that are not JSON and a test that is not in the run are passed over,
    /// and a request two running tests could have made is credited to none.
    #[test]
    fn only_project_files_and_known_tests_are_noted() {
        let root = std::env::temp_dir().join(format!(
            "supercov-support-{}-{}",
            std::process::id(),
            line!()
        ));
        let workspace = root.join("copy");
        let directory = root.join("support");
        fs::create_dir_all(&directory).unwrap();
        let at = |relative: &str| workspace.join(relative).display().to_string();
        fs::write(
            directory.join("1.jsonl"),
            [
                serde_json::json!({ "loaded": workspace.display().to_string() }),
                serde_json::json!({ "loaded": at(".supercov/runtime.mjs") }),
                serde_json::json!({ "loaded": at("node_modules/x/index.js") }),
                serde_json::json!({ "loaded": "/elsewhere/a.js" }),
                serde_json::json!({ "read": at("mock.json") }),
                serde_json::json!({ "served": "t-gone" }),
                serde_json::json!({ "unscoped": 60 }),
                serde_json::json!({ "other": 1 }),
            ]
            .iter()
            .map(|record| record.to_string())
            .chain(["not json".to_owned()])
            .collect::<Vec<_>>()
            .join("\n"),
        )
        .unwrap();
        fs::write(directory.join("notes.txt"), "{\"served\":\"t-a\"}").unwrap();
        fs::write(directory.join("noext"), "{\"served\":\"t-a\"}").unwrap();
        fs::create_dir(directory.join("dir.jsonl")).unwrap();
        let reach = read(
            &directory,
            &workspace,
            &[served("t-a", (0, 100)), served("t-b", (50, 150))],
        )
        .unwrap();
        assert_eq!(
            reach.files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["mock.json"]
        );
        assert_eq!(
            reach.files["mock.json"],
            BTreeSet::from(["t-gone".to_owned()])
        );
        assert!(reach.names.is_empty());
        // Nothing served: no reach at all; no directory: the same.
        let quiet = root.join("quiet");
        fs::create_dir(&quiet).unwrap();
        fs::write(
            quiet.join("1.jsonl"),
            serde_json::json!({ "loaded": at("a.js") }).to_string(),
        )
        .unwrap();
        assert_eq!(read(&quiet, &workspace, &[]), None);
        assert_eq!(read(&root.join("absent"), &workspace, &[]), None);
        fs::remove_dir_all(root).unwrap();
    }
}
