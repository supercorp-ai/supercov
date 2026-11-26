// What scripts/flaky-tests.mjs reads: nextest's JUnit report, a job's log,
// and which failed runs the same code passed.
import assert from 'node:assert/strict';
import test from 'node:test';
import { failedInLog, flakyInJunit, passedOnRetry } from '../../scripts/flaky-tests.mjs';

test('a JUnit report names the tests that failed and then passed, not those that failed every try', () => {
  // As nextest 0.9.140 writes it, run with `--retries 1`.
  const xml = `<?xml version="1.0" encoding="UTF-8"?>
<testsuites name="nextest-run" tests="3" failures="1" errors="0">
    <testsuite name="supercov::cli" tests="3" disabled="0" errors="0" failures="1">
        <testcase name="passes" classname="supercov::cli" time="0.011">
        </testcase>
        <testcase name="a_flaky_one" classname="supercov::cli" time="0.015">
            <flakyFailure time="0.012" message="thread &apos;a_flaky_one&apos; panicked at tests/cli.rs:4:65" type="test failure with exit code 101">first attempt
                <system-out>running 1 test</system-out>
            </flakyFailure>
        </testcase>
        <testcase name="always_fails" classname="supercov::cli" time="0.012">
            <failure message="thread &apos;always_fails&apos; panicked" type="test failure with exit code 101">always</failure>
            <rerunFailure time="0.017" message="thread &apos;always_fails&apos; panicked" type="test failure with exit code 101">always</rerunFailure>
        </testcase>
    </testsuite>
</testsuites>`;
  assert.deepEqual(flakyInJunit(xml), [
    { binary: 'supercov::cli', test: 'a_flaky_one', failures: 1, message: "thread 'a_flaky_one' panicked at tests/cli.rs:4:65" },
  ]);
  assert.deepEqual(flakyInJunit('<testsuites></testsuites>'), []);
});

test("a job's log names the tests that failed, as libtest, nextest and a gate say so", () => {
  const log = [
    '2026-11-26T06:16:46.6519010Z test a_playwright_plan_has_spans_and_overlapping_tests_stay_parallel ... FAILED',
    '2026-11-26T06:16:46.6519010Z test passes ... ok',
    '2026-11-22T16:06:01.4609738Z         FAIL [   7.818s] ( 155/1404) supercov::cli mutants_test_narrows_shares_and_gates_what_it_asks_about',
    '2026-11-22T16:06:01.4609738Z   TRY 1 FAIL [   0.012s] (───) supercov::cli a_retried_one',
    '2026-11-22T16:06:01.4609738Z      TIMEOUT [ 600.002s] supercov-engine process_supervision::tests::hangs',
    '2026-11-22T16:06:01.4609738Z \u001b[31mnot ok 3 - a gate that failed\u001b[0m',
    '2026-11-22T16:06:01.4609738Z test result: FAILED. 0 passed; 1 failed',
  ].join('\n');
  assert.deepEqual(failedInLog(log), [
    'a gate that failed',
    'a_playwright_plan_has_spans_and_overlapping_tests_stay_parallel',
    'a_retried_one',
    'mutants_test_narrows_shares_and_gates_what_it_asks_about',
    'process_supervision::tests::hangs',
  ]);
});

test('a failure is flaky where a later attempt or another run of the same commit passed', () => {
  const run = (id, workflow, sha, conclusion, attempts) => ({ id, workflow, head_sha: sha, conclusion, run_attempt: attempts });
  const runs = [
    run(1, 'self-coverage.yml', 'aaa', 'success', 3),
    run(2, 'ci.yml', 'bbb', 'failure', 1),
    run(3, 'ci.yml', 'bbb', 'success', 1),
    run(4, 'ci.yml', 'ccc', 'failure', 2),
    run(5, 'self-coverage.yml', 'bbb', 'failure', 1),
    run(6, 'ci.yml', 'ddd', 'cancelled', 1),
  ];
  assert.deepEqual(passedOnRetry(runs).map(({ run, attempt }) => [run.id, attempt]), [[1, 1], [1, 2], [2, 1]]);
});
