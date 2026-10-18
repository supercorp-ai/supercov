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
    /// The attempt's own id, as its runner's scope names it: what a take at
    /// the test's edge is recorded under.
    pub attempt_id: Option<String>,
}

/// One phase starting: in which process (its transport), where in its
/// records, and of which attempt (worker, test, retry).
pub type PhaseStart = (String, usize, Attempt);

/// A test attempt: worker, test, retry.
pub type Attempt = (String, String, usize);

/// The suite's own test attempts, in the order each process ran them: what
/// a request to a server the suite runs in its own process -- a live server
/// a browser or the test itself calls -- is judged by. A process writes its
/// records in order, so the attempt whose phase began last before the
/// request did is the one running when it came: exactly, where the attempts
/// were run one after another, and no one's where they interleave.
pub struct InProcess {
    /// By transport: where each phase began and whose, and whether the
    /// process ran attempts at once.
    by_transport: BTreeMap<String, (Vec<(usize, Attempt)>, bool)>,
}

impl InProcess {
    pub fn new(phases: &[PhaseStart]) -> Self {
        let mut by_transport = BTreeMap::<String, (Vec<_>, bool)>::new();
        for (transport, seq, attempt) in phases {
            by_transport
                .entry(transport.clone())
                .or_default()
                .0
                .push((*seq, attempt.clone()));
        }
        for (starts, interleaved) in by_transport.values_mut() {
            starts.sort();
            let mut left = BTreeSet::new();
            for pair in starts.windows(2) {
                if pair[0].1 != pair[1].1 {
                    left.insert(pair[0].1.clone());
                    *interleaved |= left.contains(&pair[1].1);
                }
            }
        }
        Self { by_transport }
    }

    pub fn owner(&self, transport: &str, seq: usize) -> Option<&Attempt> {
        let (starts, interleaved) = self.by_transport.get(transport)?;
        if *interleaved {
            return None;
        }
        let before = starts.partition_point(|(start, _)| *start < seq);
        before.checked_sub(1).map(|index| &starts[index].1)
    }
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
    read_journals(evidence_directory, |name| {
        name.starts_with("playwright-worker-")
    })
}

/// Each test attempt any runner reported -- node:test, Vitest, Jest, Mocha,
/// AVA, tap, Playwright -- read from the evidence journals they all write.
pub fn read_attempts(evidence_directory: &Path) -> Vec<ServedTest> {
    read_journals(evidence_directory, |_| true)
}

fn read_journals(evidence_directory: &Path, wanted: impl Fn(&str) -> bool) -> Vec<ServedTest> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "camelCase")]
    struct Scope {
        worker_id: String,
        #[serde(default)]
        attempt_id: Option<String>,
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
        /// The attempt's first and last moment, from fixtures that record it.
        #[serde(default)]
        span: Option<[i64; 2]>,
        /// `background` for a record of work between tests: no test.
        #[serde(default)]
        role: Option<String>,
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
                .is_some_and(|name| wanted(name) && name.ends_with(".mcdc.jsonl"))
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
            .filter(|attempt| attempt.role.as_deref().is_none_or(|role| role == "test"))
        {
            let worker = format!("playwright:{}", attempt.scope.worker_id);
            // The whole attempt where the fixture recorded it: a test runs
            // between its actions too, and a span of its actions alone is a
            // few milliseconds a sweep of the server's clock can miss.
            // Otherwise, its first and last phase.
            let (start, end) = match attempt.span {
                Some([start, end]) => (Some(start), Some(end)),
                None => (
                    attempt.phases.iter().map(|phase| phase.started_at_ms).min(),
                    attempt
                        .phases
                        .iter()
                        .map(|phase| phase.ended_at_ms.unwrap_or(phase.started_at_ms))
                        .max(),
                ),
            };
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
                    attempt_id: attempt.scope.attempt_id,
                });
        }
    }
    served.into_values().collect()
}

#[cfg(test)]
mod tests {

    #[test]
    fn a_test_is_running_for_its_whole_attempt_where_the_fixture_says_so() {
        let directory = std::env::temp_dir().join(format!(
            "supercov-served-span-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let attempt = |id: &str, span: Option<[i64; 2]>| {
            let mut line = serde_json::json!({
                "testId": id,
                "scope": {"workerId": "w0"},
                "test": id,
                "testFile": "a.spec.js",
                "title": id,
                "retry": 0,
                "status": "passed",
                "provenance": {"runner": "playwright", "kind": "e2e", "source": "explicit"},
                "phases": [{"id": "p", "kind": "action", "operation": "get", "startedAtMs": 1050, "endedAtMs": 1053}],
            });
            if let Some(span) = span {
                line["span"] = serde_json::json!(span);
            }
            line.to_string()
        };
        std::fs::write(
            directory.join("playwright-worker-0.mcdc.jsonl"),
            format!(
                "{}\n{}\n",
                attempt("spanned", Some([1000, 1110])),
                attempt("older", None)
            ),
        )
        .unwrap();
        let served = read(&directory)
            .into_iter()
            .map(|test| (test.test_id, test.window))
            .collect::<std::collections::BTreeMap<_, _>>();
        // The attempt, pauses and all; a fixture that records no span is
        // judged by its phases, as before.
        assert_eq!(served["spanned"], Some((1000, 1110)));
        assert_eq!(served["older"], Some((1050, 1053)));
        std::fs::remove_dir_all(directory).unwrap();
    }

    use super::*;

    // A request to a server the suite runs itself is the test's whose phase
    // began last before it in that process: by record, which a millisecond
    // clock cannot tell apart when a test ends, the next starts and the
    // request comes all in one millisecond.
    #[test]
    fn a_request_in_process_is_the_test_running_before_it() {
        let attempt = |test: &str| ("main".to_owned(), test.to_owned(), 0);
        let phases = [
            ("a".to_owned(), 1, attempt("one")),
            ("a".to_owned(), 2, attempt("one")),
            ("a".to_owned(), 5, attempt("one")),
            ("a".to_owned(), 6, attempt("two")),
            ("a".to_owned(), 7, attempt("two")),
        ];
        let order = InProcess::new(&phases);
        assert_eq!(order.owner("a", 0), None, "before any test");
        assert_eq!(order.owner("a", 3), Some(&attempt("one")));
        assert_eq!(
            order.owner("a", 6),
            Some(&attempt("one")),
            "as the next test begins"
        );
        assert_eq!(order.owner("a", 8), Some(&attempt("two")));
        assert_eq!(order.owner("b", 3), None, "another process ran no test");
        // Tests that ran at once in one process are no one's.
        let interleaved = InProcess::new(&[
            ("a".to_owned(), 1, attempt("one")),
            ("a".to_owned(), 2, attempt("two")),
            ("a".to_owned(), 3, attempt("one")),
        ]);
        assert_eq!(interleaved.owner("a", 4), None);
    }
}
