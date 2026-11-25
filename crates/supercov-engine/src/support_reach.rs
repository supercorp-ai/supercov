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
#[cfg_attr(test, derive(schemars::JsonSchema))]
pub struct SupportReach {
    /// Each project file a serving process loaded or read, relative to the
    /// project, and the tests (by id) those processes served.
    pub files: BTreeMap<String, BTreeSet<String>>,
    /// Each of those tests' name, as the run names it.
    #[serde(default)]
    pub names: BTreeMap<String, String>,
    /// Every project file any Node process of the run loaded or read --
    /// the test runner, its workers, the servers -- served a request or
    /// not: the run's evidence of what its tests could load. None for a run
    /// recorded before it was kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loaded: Option<BTreeSet<String>>,
}

impl SupportReach {
    /// Leaves out the files the measured command wrote that the project does
    /// not track, and returns them. Such a file -- a local stack's settings,
    /// written by the harness with each machine's ports and keys -- is what
    /// the command made, not something the checkout holds: a later checkout
    /// cannot change it under the run, and runs of the command on several
    /// machines read it different. Recorded, a suite measured in parts on
    /// several machines could not be merged. A tracked file the command
    /// rewrote (a mock's data it saves) is kept, as is every file where git
    /// cannot say what it tracks.
    pub fn leave_out_generated(&mut self, root: &Path, written: &BTreeSet<String>) -> Vec<String> {
        let candidates = self
            .files
            .keys()
            .filter(|file| written.contains(*file))
            .cloned()
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            return Vec::new();
        }
        let Some(tracked) = tracked_of(root, &candidates) else {
            return Vec::new();
        };
        let generated = candidates
            .into_iter()
            .filter(|file| !tracked.contains(file))
            .collect::<Vec<_>>();
        self.files.retain(|file, _| !generated.contains(file));
        generated
    }

    /// One reach from several (a merge's runs).
    pub fn extend(&mut self, other: SupportReach) {
        for (file, tests) in other.files {
            self.files.entry(file).or_default().extend(tests);
        }
        self.names.extend(other.names);
        if let Some(loaded) = other.loaded {
            self.loaded.get_or_insert_with(BTreeSet::new).extend(loaded);
        }
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
    let mut loaded = BTreeSet::new();
    for process in processes {
        loaded.extend(process.files.iter().cloned());
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
    // What any process loaded is the run's own evidence, whoever it served.
    if !loaded.is_empty() {
        any = true;
        reach.loaded = Some(loaded);
    }
    any.then_some(reach)
}

/// Which of `files` git tracks in the project at `root`; none where git
/// cannot say (no repository, no git).
fn tracked_of(root: &Path, files: &[String]) -> Option<BTreeSet<String>> {
    use crate::fault_process::Launch;
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args(["ls-files", "-z", "--"])
        .args(files)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .launch_output()
        .ok()
        .filter(|output| output.status.success())?;
    Some(
        String::from_utf8_lossy(&output.stdout)
            .split('\0')
            .filter(|file| !file.is_empty())
            .map(str::to_owned)
            .collect(),
    )
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

    /// Nothing recorded is no reach: an empty directory, an empty record,
    /// and a server that loaded no file reach nothing; merging nothing in
    /// changes nothing.
    #[test]
    fn nothing_recorded_reaches_nothing() {
        let root = std::env::temp_dir().join(format!(
            "supercov-support-empty-{}-{}",
            std::process::id(),
            line!()
        ));
        let directory = root.join("support");
        fs::create_dir_all(&directory).unwrap();
        assert!(read(&directory, &root, &[]).is_none());
        fs::write(directory.join("1-a.jsonl"), "").unwrap();
        assert!(read(&directory, &root, &[]).is_none());
        fs::write(
            directory.join("2-b.jsonl"),
            format!("{}\n", serde_json::json!({ "served": "t-pay" })),
        )
        .unwrap();
        let reach = read(&directory, &root, &[served("t-pay", (0, 10))]).unwrap();
        assert!(reach.files.is_empty());
        let mut merged = reach.clone();
        merged.extend(SupportReach::default());
        assert_eq!(merged.files, reach.files);
        fs::remove_dir_all(&root).unwrap();
    }

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
        // The worker loaded it all the same: the run's evidence says so.
        assert!(reach.loaded.as_ref().unwrap().contains("tests/helpers.ts"));
        // Merged, what either loaded; one recorded before it was kept adds
        // nothing to it.
        let mut merged = SupportReach::default();
        merged.extend(SupportReach::default());
        assert_eq!(merged.loaded, None);
        merged.extend(reach.clone());
        assert_eq!(merged.loaded, reach.loaded);
        assert_eq!(
            reach.names.get("t-refund").map(String::as_str),
            Some("t-refund")
        );
        fs::remove_dir_all(root).unwrap();
    }

    /// A file the command wrote that git does not track is left out of the
    /// reach; one git tracks stays, rewritten or not, and so does every file
    /// where there is no repository to ask.
    #[test]
    fn generated_files_are_left_out() {
        let root = std::env::temp_dir().join(format!(
            "supercov-support-{}-{}",
            std::process::id(),
            line!()
        ));
        fs::create_dir_all(root.join("tests/stack")).unwrap();
        let tests = BTreeSet::from(["t-a".to_owned()]);
        let reach = SupportReach {
            files: [
                "tests/stack/.env.local",
                "tests/mock.json",
                "tests/mock.mjs",
            ]
            .into_iter()
            .map(|file| (file.to_owned(), tests.clone()))
            .collect(),
            names: BTreeMap::new(),
            loaded: None,
        };
        let written = ["tests/stack/.env.local", "tests/mock.json"]
            .into_iter()
            .map(str::to_owned)
            .collect::<BTreeSet<_>>();
        // Outside a repository: git cannot say, and everything stays.
        // (The temporary directory is in no repository.)
        let mut outside = reach.clone();
        assert!(outside.leave_out_generated(&root, &written).is_empty());
        assert_eq!(outside, reach);
        // Nothing written: nothing to ask.
        let mut unwritten = reach.clone();
        assert!(
            unwritten
                .leave_out_generated(&root, &BTreeSet::new())
                .is_empty()
        );
        assert_eq!(unwritten, reach);
        let git = |arguments: &[&str]| {
            assert!(
                std::process::Command::new("git")
                    .arg("-C")
                    .arg(&root)
                    .args(arguments)
                    .output()
                    .unwrap()
                    .status
                    .success()
            );
        };
        git(&["init", "-q"]);
        fs::write(root.join("tests/mock.json"), "{}").unwrap();
        fs::write(root.join("tests/mock.mjs"), "").unwrap();
        fs::write(root.join("tests/stack/.env.local"), "KEY=1").unwrap();
        git(&["add", "tests/mock.json", "tests/mock.mjs"]);
        let mut inside = reach.clone();
        assert_eq!(
            inside.leave_out_generated(&root, &written),
            ["tests/stack/.env.local"]
        );
        assert_eq!(
            inside.files.keys().map(String::as_str).collect::<Vec<_>>(),
            ["tests/mock.json", "tests/mock.mjs"]
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
        // What every process loaded, a test worker's too.
        assert_eq!(reach.loaded, Some(BTreeSet::from(["mock.json".to_owned()])));
        // Nothing served: what was loaded alone; no directory: nothing.
        let quiet = root.join("quiet");
        fs::create_dir(&quiet).unwrap();
        fs::write(
            quiet.join("1.jsonl"),
            serde_json::json!({ "loaded": at("a.js") }).to_string(),
        )
        .unwrap();
        assert_eq!(
            read(&quiet, &workspace, &[]),
            Some(SupportReach {
                loaded: Some(BTreeSet::from(["a.js".to_owned()])),
                ..SupportReach::default()
            })
        );
        assert_eq!(read(&root.join("absent"), &workspace, &[]), None);
        fs::remove_dir_all(root).unwrap();
    }
}
