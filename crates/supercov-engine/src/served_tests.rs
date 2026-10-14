//! The browser tests a measured server answered: what the browser runner
//! reported for each attempt, which a Python or Ruby server's run takes its
//! tests from, and the rule that gives a request naming no test to one.

use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

use crate::coverage_report::TestProvenance;

/// The worker a server's runtime records a request that names no test
/// under, numbered within its process.
pub const REQUEST_WORKER: &str = "supercov-request";

/// A browser test whose requests a measured server answered: what the
/// browser runner reported for it, which no runner of the server's language
/// saw.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServedTest {
    /// As the server's runtime names the worker: `playwright:<worker id>`.
    pub worker: String,
    pub test_id: String,
    pub retry: usize,
    /// The test's name, title and file as the browser run gives them.
    pub name: String,
    pub title: String,
    pub file: String,
    pub status: String,
    pub expected_status: String,
    pub provenance: TestProvenance,
    /// When the attempt ran, in milliseconds since the epoch, first and
    /// last moment: what a request that named no test is judged by.
    pub window: Option<(i64, i64)>,
}

/// The attempt a request that named no test belongs to: the one running when
/// it came, if only one was, among the attempts its process answered a
/// request of where it answered any -- with a server per worker, the
/// worker's test, whatever other workers ran meanwhile -- and among every
/// attempt otherwise.
pub fn owner<'a>(
    served: &'a [ServedTest],
    answered: Option<&BTreeSet<(String, String, usize)>>,
    at: i64,
) -> Option<&'a ServedTest> {
    let mut running = served.iter().filter(|test| {
        answered.is_none_or(|attempts| {
            attempts.contains(&(test.worker.clone(), test.test_id.clone(), test.retry))
        }) && test
            .window
            .is_some_and(|(start, end)| start <= at && at <= end)
    });
    match (running.next(), running.next()) {
        (Some(only), None) => Some(only),
        _ => None,
    }
}

/// Each browser test attempt the suite reported, read from the Playwright
/// fixture's evidence lines.
pub fn read(evidence_directory: &Path) -> Vec<ServedTest> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Scope {
        worker_id: String,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Attempt {
        test_id: String,
        scope: Scope,
        test: String,
        test_file: String,
        title: String,
        retry: usize,
        status: String,
        expected_status: Option<String>,
        provenance: crate::coverage_report::TestProvenance,
        #[serde(default)]
        phases: Vec<Phase>,
    }
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Phase {
        started_at_ms: i64,
        ended_at_ms: Option<i64>,
    }
    let mut served = BTreeMap::new();
    let Ok(entries) = std::fs::read_dir(evidence_directory) else {
        return Vec::new();
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("playwright-worker-") && name.ends_with(".mcdc.jsonl")
                })
        })
        .collect::<Vec<_>>();
    files.sort();
    for file in files {
        let Ok(text) = std::fs::read_to_string(&file) else {
            continue;
        };
        for attempt in text
            .lines()
            .filter_map(|line| serde_json::from_str::<Attempt>(line).ok())
        {
            let worker = format!("playwright:{}", attempt.scope.worker_id);
            let start = attempt.phases.iter().map(|phase| phase.started_at_ms).min();
            let end = attempt
                .phases
                .iter()
                .map(|phase| phase.ended_at_ms.unwrap_or(phase.started_at_ms))
                .max();
            served
                .entry((worker.clone(), attempt.test_id.clone(), attempt.retry))
                .or_insert(ServedTest {
                    worker,
                    test_id: attempt.test_id,
                    retry: attempt.retry,
                    name: attempt.test,
                    title: attempt.title,
                    file: attempt.test_file,
                    status: attempt.status,
                    expected_status: attempt.expected_status.unwrap_or_else(|| "passed".into()),
                    provenance: attempt.provenance,
                    window: start.zip(end),
                });
        }
    }
    served.into_values().collect()
}
