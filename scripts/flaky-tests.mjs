#!/usr/bin/env node
// Flaky tests, named where someone will fix them: a test that fails and then
// passes on the same code says nothing of the change under test, and a run
// that hides it (a retry, a rerun of the job) also hides that it needs fixing.
// Each is added to the open issue labelled `flaky-tests` (made the first time
// one is found), one comment per run, with what failed and where.
//
//   node scripts/flaky-tests.mjs collect <out.json> <junit.xml>...
//     The tests nextest ran again and passed (`--retries`; a `flakyFailure`
//     in its JUnit report), in the step summary and in <out.json>, written
//     only when there are some. A test that failed every try fails the job
//     as before.
//   node scripts/flaky-tests.mjs report <directory>
//     Every <out.json> under <directory> (the run's `flaky-*` artifacts), as
//     one comment on the tracking issue. Needs GH_TOKEN, GITHUB_REPOSITORY
//     and GITHUB_RUN_ID.
//   node scripts/flaky-tests.mjs census [hours]
//     The runs of the last <hours> (24) of CI, Compatibility, Self Coverage,
//     React and Next compatibility whose failure a later attempt of the same
//     run, or another run of the same commit, did not repeat: the jobs that
//     failed and the tests their logs say failed, as one comment on the
//     tracking issue. What a rerun made green, Self Coverage's slices among
//     them, which retry nothing.
import { appendFileSync, existsSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { execFileSync } from 'node:child_process';
import { join, resolve } from 'node:path';

export const LABEL = 'flaky-tests';
const TITLE = 'Flaky tests';
const WORKFLOWS = ['ci.yml', 'compatibility.yml', 'self-coverage.yml', 'react-compatibility.yml', 'next-compatibility.yml'];

const unescape = (text) => text
  .replace(/&lt;/g, '<').replace(/&gt;/g, '>').replace(/&quot;/g, '"').replace(/&apos;/g, "'").replace(/&amp;/g, '&');

/**
 * The flaky tests of a nextest JUnit report: each test case with a
 * `flakyFailure` (it failed, then passed when run again), with how often it
 * failed and the first failure's message.
 */
export function flakyInJunit(xml) {
  const flaky = [];
  for (const [, attributes, body] of xml.matchAll(/<testcase\b([^>]*)>([\s\S]*?)<\/testcase>/g)) {
    const failures = [...body.matchAll(/<flakyFailure\b[^>]*?message="([^"]*)"/g)];
    if (!failures.length || /<failure\b/.test(body)) continue;
    const attribute = (name) => unescape(attributes.match(new RegExp(`\\b${name}="([^"]*)"`))?.[1] ?? '');
    flaky.push({ binary: attribute('classname'), test: attribute('name'), failures: failures.length, message: unescape(failures[0][1]) });
  }
  return flaky;
}

/**
 * The tests a job's log says failed: libtest's `test NAME ... FAILED`,
 * nextest's `FAIL [ time ] binary NAME` (a try that was run again too), and
 * a gate's or node:test's `not ok N - NAME`.
 */
export function failedInLog(log) {
  const names = new Set();
  for (const raw of log.split('\n')) {
    // Each line of a job's log starts with its timestamp.
    const line = raw.replace(/\x1b\[[0-9;]*m/g, '').replace(/^\d{4}-\d\d-\d\dT[\d:.]+Z /, '');
    const libtest = line.match(/^test (\S+) \.\.\. FAILED$/);
    const nextest = line.match(/^\s*(?:TRY \d+ )?(?:FAIL|TIMEOUT|SIGSEGV|SIGABRT|SIGKILL|ABORT) \[[^\]]*\](?: \([^)]*\))? \S+ (\S+)\s*$/);
    const tap = line.match(/^not ok \d+ - (.+)$/);
    const name = libtest?.[1] ?? nextest?.[1] ?? tap?.[1];
    if (name) names.add(name.trim());
  }
  return [...names].sort();
}

/**
 * Which failed attempts of `runs` the same code then passed: each run's
 * attempts before its last, where that last succeeded, and a failed run
 * where another run of the same workflow and commit succeeded. `runs` are
 * GitHub's run records ({id, workflow, head_sha, conclusion, run_attempt}).
 */
export function passedOnRetry(runs) {
  const passed = new Set(runs.filter((run) => run.conclusion === 'success').map((run) => `${run.workflow}\0${run.head_sha}`));
  const retried = [];
  for (const run of runs) {
    if (run.conclusion === 'success') {
      for (let attempt = 1; attempt < run.run_attempt; attempt += 1) retried.push({ run, attempt });
      continue;
    }
    if (run.conclusion === 'failure' && passed.has(`${run.workflow}\0${run.head_sha}`)) retried.push({ run, attempt: run.run_attempt });
  }
  return retried;
}

function gh(args, input) {
  return execFileSync('gh', args, { encoding: 'utf8', input, maxBuffer: 1 << 28 });
}

function api(path) {
  return JSON.parse(gh(['api', path]));
}

/** The open tracking issue's number, made (and its label) if there is none. */
function trackingIssue(repository) {
  const open = JSON.parse(gh(['issue', 'list', '--repo', repository, '--label', LABEL, '--state', 'open', '--json', 'number', '--limit', '1']));
  if (open.length) return open[0].number;
  try {
    gh(['label', 'create', LABEL, '--repo', repository, '--color', 'd93f0b', '--description', 'A test that failed and then passed on the same code']);
  } catch {
    // Made already.
  }
  const body = [
    'Tests that failed and then passed on the same code, one comment per run that saw some: a nextest retry that passed (CI, Compatibility), or a run or job that passed when run again (`scripts/flaky-tests.mjs census`, daily).',
    '',
    'A flaky test is fixed at its cause: an event or a condition to wait for in place of a time, a port of its own, a product verdict that does not depend on how busy the machine is. Close this issue once every test named here is fixed; the next flake opens a new one.',
  ].join('\n');
  const url = gh(['issue', 'create', '--repo', repository, '--title', TITLE, '--label', LABEL, '--body-file', '-'], body).trim();
  return Number(url.split('/').pop());
}

function comment(repository, text) {
  // FLAKY_TESTS_DRY_RUN=1: printed, nothing posted.
  if (process.env.FLAKY_TESTS_DRY_RUN === '1') {
    console.log(text);
    return;
  }
  const issue = trackingIssue(repository);
  gh(['issue', 'comment', String(issue), '--repo', repository, '--body-file', '-'], text);
  console.log(`[flaky-tests] added to issue #${issue}`);
}

function filesUnder(directory) {
  if (!existsSync(directory)) return [];
  return readdirSync(directory).flatMap((name) => {
    const path = join(directory, name);
    return statSync(path).isDirectory() ? filesUnder(path) : [path];
  });
}

const [command, ...rest] = process.argv.slice(2);
if (process.argv[1] && resolve(process.argv[1]) === resolve(import.meta.filename)) {
  if (command === 'collect') {
    const [out, ...reports] = rest;
    const flaky = reports.filter((path) => existsSync(path)).flatMap((path) => flakyInJunit(readFileSync(path, 'utf8')));
    const lines = flaky.map((one) => `- \`${one.binary} ${one.test}\`: failed ${one.failures} time(s), then passed: ${one.message}`);
    const summary = flaky.length
      ? ['### Flaky tests', '', `${flaky.length} test(s) failed and then passed when run again. The job passes; the test needs fixing:`, '', ...lines, ''].join('\n')
      : '';
    if (summary) {
      console.log(summary);
      if (process.env.GITHUB_STEP_SUMMARY) appendFileSync(process.env.GITHUB_STEP_SUMMARY, `${summary}\n`);
      writeFileSync(out, JSON.stringify({ job: process.env.FLAKY_JOB ?? '', flaky }, null, 2));
    }
  } else if (command === 'report') {
    const [directory] = rest;
    const jobs = filesUnder(directory).filter((path) => path.endsWith('.json')).map((path) => JSON.parse(readFileSync(path, 'utf8')));
    const lines = jobs.flatMap(({ job, flaky }) => flaky.map((one) => `- \`${one.binary} ${one.test}\` in ${job}: failed ${one.failures} time(s), then passed. ${one.message}`));
    if (!lines.length) {
      console.log('[flaky-tests] no flaky test in this run');
    } else {
      const repository = process.env.GITHUB_REPOSITORY;
      const run = `${process.env.GITHUB_SERVER_URL ?? 'https://github.com'}/${repository}/actions/runs/${process.env.GITHUB_RUN_ID}`;
      const ref = process.env.GITHUB_HEAD_REF || process.env.GITHUB_REF_NAME || '';
      comment(repository, [`**${process.env.GITHUB_WORKFLOW ?? 'A run'}** [${process.env.GITHUB_RUN_ID}](${run}) on \`${ref}\` (${(process.env.GITHUB_SHA ?? '').slice(0, 8)}): passed only on a retry`, '', ...lines].join('\n'));
    }
  } else if (command === 'census') {
    const repository = process.env.GITHUB_REPOSITORY;
    const hours = Number(rest[0] ?? 24);
    const since = new Date(Date.now() - hours * 3_600_000).toISOString();
    const runs = WORKFLOWS.flatMap((workflow) =>
      api(`repos/${repository}/actions/workflows/${workflow}/runs?per_page=100&created=>=${since}`).workflow_runs.map((run) => ({ ...run, workflow })));
    const lines = [];
    for (const { run, attempt } of passedOnRetry(runs)) {
      const jobs = api(`repos/${repository}/actions/runs/${run.id}/attempts/${attempt}/jobs?per_page=100`).jobs.filter((job) => job.conclusion === 'failure');
      for (const job of jobs) {
        let tests = [];
        try {
          tests = failedInLog(gh(['api', '--allow-escape-sequences', `repos/${repository}/actions/jobs/${job.id}/logs`]));
        } catch {
          // Its log is gone: the job alone is named.
        }
        const step = job.steps?.find((one) => one.conclusion === 'failure')?.name ?? 'a step';
        lines.push(`- ${run.workflow} [${run.id}](${run.html_url}/attempts/${attempt}) attempt ${attempt} on \`${run.head_branch}\` (${run.head_sha.slice(0, 8)}), **${job.name}**: ${tests.length ? tests.map((name) => `\`${name}\``).join(', ') : `failed at "${step}"`}`);
      }
    }
    if (!lines.length) {
      console.log(`[flaky-tests] no failure in the last ${hours} h passed when run again`);
    } else {
      console.log(lines.join('\n'));
      comment(repository, [`**Census of the last ${hours} h**: failures the same code did not repeat when run again`, '', ...lines].join('\n'));
    }
  } else {
    console.error('usage: flaky-tests.mjs collect <out.json> <junit.xml>... | report <directory> | census [hours]');
    process.exit(2);
  }
}
